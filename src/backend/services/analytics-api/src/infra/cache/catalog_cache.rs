//! Catalog cache layer (`cpt-metric-cat-component-cache-layer`).
//!
//! Server-side cache front for `POST /catalog/get_metrics` per DESIGN §3.2 /
//! §3.6 (`cpt-metric-cat-seq-catalog-read`). Carries the read-latency hit-path
//! NFR and the cross-replica invalidation NFR.
//!
//! ## Cache key
//!
//! Canonical key: `cat:v1:{tenant_id}:{role_slug_or_empty}:{team_id_or_empty}`,
//! with every segment URL-safe-encoded. `tenant_id` is rendered as the
//! lowercase hyphenated UUID (deterministic regardless of input casing).
//! `role_slug` / `team_id` use the **empty-string sentinel** when absent — the
//! sentinel is identical to the DB UNIQUE-composite convention (#519) and
//! eliminates the "two callers in different tenants generate the same key"
//! collision class. The encoder percent-encodes any byte outside
//! `[A-Za-z0-9_\-]`; a hostile or accidental `role_slug = "a:b"` becomes
//! `a%3Ab` and cannot bleed into the tenant or team segments.
//!
//! ## Invalidation contract
//!
//! - [`InvalidateMode::Standard`] — tenant-prefix purge
//!   (`SCAN cat:v1:{tenant}:* + UNLINK`). NEVER `FLUSHDB` (the Redis instance
//!   is shared with `person_aliases:*` from Identity Resolution; FLUSHDB would
//!   clobber sibling namespaces).
//! - [`InvalidateMode::Lock`] — same prefix purge, **plus** opens a 5 s
//!   synchronous-resolver-bypass window for that tenant. Sized at
//!   `2 × cpt-metric-cat-nfr-cross-replica-invalidation`-p99 (= 2 × 2 s).
//!   While the window is open, [`CatalogCache::should_skip`] returns `true`
//!   and the reader skips the cache entirely — closing the stale-policy gap
//!   on compliance-critical lock writes during the broadcast window.
//!
//! ## Defense-in-depth on hydrate
//!
//! Cached payloads carry `tenant_id`; the cache re-asserts it on read against
//! the requesting tenant. A mismatch is a backend-misconfiguration smell, not
//! a normal failure mode — it logs a security warning and returns a miss so
//! the resolver repopulates from authoritative state.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use redis::AsyncCommands;
use uuid::Uuid;

use crate::domain::catalog::response::CatalogResponse;

/// Canonical Redis-key prefix for the catalog cache. Every catalog entry's
/// key is `cat:v1:{tenant_id}:{role_slug_or_empty}:{team_id_or_empty}` per
/// DESIGN §3.2; this constant is the prefix the seed-migration flush and
/// admin-write invalidations both walk.
pub const CACHE_KEY_PREFIX: &str = "cat:v1:";

/// Default per-entry TTL. PRD §5.3 `cpt-metric-cat-fr-cache` mandates 5 minutes;
/// admin writes invalidate ahead of TTL so users don't observe "I changed the
/// threshold, nothing happened" stale-read gap.
pub const DEFAULT_TTL: Duration = Duration::from_mins(5);

/// Lock-event synchronous-bypass window — `2 ×` the cross-replica-invalidation
/// NFR p99 (2 s) per DESIGN §3.2 cache-layer.
pub const LOCK_BYPASS_WINDOW: Duration = Duration::from_secs(5);

/// Invalidation mode. `Lock` writes additionally open the bypass window;
/// every other admin write uses `Standard`.
///
/// `#[allow(dead_code)]` on `Standard` until admin-crud (#525) lands and
/// starts calling `invalidate(tenant, Standard)` on threshold writes. The
/// `Lock` variant is exercised by the tests in this module and `reader.rs`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidateMode {
    Standard,
    Lock,
}

/// Catalog cache trait — abstracts the Redis (or future pub-sub) backend so
/// the reader and admin paths don't depend on a concrete client.
#[async_trait]
pub trait CatalogCache: Send + Sync {
    /// Fetch a cached response. The cache re-asserts the embedded `tenant_id`
    /// against `requesting_tenant_id`; a mismatch logs a security warning and
    /// returns `None` (cache miss). Returns `Ok(None)` on miss; surfaces
    /// connection / decode failures as `Err`.
    ///
    /// # Errors
    ///
    /// Surfaces Redis transport and JSON decode failures. Callers MUST treat
    /// errors as "miss + degrade gracefully" — a Redis blip MUST NOT 5xx the
    /// read endpoint.
    async fn get(
        &self,
        key: &str,
        requesting_tenant_id: Uuid,
    ) -> anyhow::Result<Option<CatalogResponse>>;

    /// Store a response under `key` with `ttl`.
    ///
    /// # Errors
    ///
    /// Surfaces Redis transport / encode failures. As with [`Self::get`],
    /// callers treat errors as "skip cache; serve resolver result anyway".
    async fn put(&self, key: &str, payload: &CatalogResponse, ttl: Duration) -> anyhow::Result<()>;

    /// Tenant-prefix purge (`SCAN cat:v1:{tenant_id}:* + UNLINK`). When `mode`
    /// is [`InvalidateMode::Lock`], also opens the 5 s synchronous-bypass
    /// window for that tenant (see [`LOCK_BYPASS_WINDOW`]).
    ///
    /// First call site is admin-crud (#525); the trait method is exercised by
    /// tests today so the dead-code annotation only applies until that PR
    /// lands.
    ///
    /// # Errors
    ///
    /// Surfaces Redis transport failures.
    #[allow(dead_code)]
    async fn invalidate(&self, tenant_id: Uuid, mode: InvalidateMode) -> anyhow::Result<()>;

    /// Purge every key under [`CACHE_KEY_PREFIX`]. Used by the seed migration
    /// at boot and by `analytics-api migrate` after applying migrations.
    ///
    /// # Errors
    ///
    /// Surfaces Redis transport failures.
    async fn flush_all(&self) -> anyhow::Result<()>;

    /// True when the lock-bypass window for `tenant_id` is still open and the
    /// reader MUST skip the cache for that tenant. Synchronous and lock-free
    /// on the hot read path.
    fn should_skip(&self, tenant_id: Uuid) -> bool;
}

/// Build the canonical cache key. `None` and `Some("")` map to the same key
/// (canonical empty-string sentinel — see module docs).
#[must_use]
pub fn cache_key(tenant_id: Uuid, role_slug: Option<&str>, team_id: Option<&str>) -> String {
    let role = url_safe_encode(role_slug.unwrap_or(""));
    let team = url_safe_encode(team_id.unwrap_or(""));
    // `Uuid::hyphenated` is always 36 lowercase chars — deterministic regardless
    // of how the caller spelled the UUID upstream.
    format!(
        "{CACHE_KEY_PREFIX}{tenant_id}:{role}:{team}",
        tenant_id = tenant_id.hyphenated()
    )
}

/// Percent-encode bytes outside the URL-safe whitelist `[A-Za-z0-9_\-]`.
/// Empty input round-trips to empty output. Output is ASCII-only so the
/// composed key is always a valid Redis key string.
fn url_safe_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let b = *byte;
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
            out.push(b as char);
        } else {
            // Uppercase hex per RFC 3986 §2.1 ("hexadecimal digits ... uppercase
            // recommended"). Lowercase would still be valid but mixing cases
            // across call sites is the silent-cache-miss footgun this constant
            // prevents.
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    out
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

// ── Skip-until map (lock-bypass window) ──────────────────────────────────

/// Shared in-process map of "tenant → bypass-until instant". Reads are cheap
/// (the lock is held only across a single `HashMap::get`). Mutations occur on
/// admin writes (rare), so contention is bounded.
#[derive(Default)]
struct SkipUntilMap {
    inner: Mutex<HashMap<Uuid, Instant>>,
}

impl SkipUntilMap {
    fn arm(&self, tenant_id: Uuid, until: Instant) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                // Mutex poisoning is a panic-in-handler smell, not a normal
                // condition. Take the inner map anyway so the bypass map
                // doesn't permanently degrade after one panic — the lock is
                // semantically a "latest-write-wins" register, no invariant
                // is corrupted by a poison.
                tracing::warn!(
                    "catalog_cache: skip-until mutex was poisoned; \
                     reusing inner map (no invariant lost)"
                );
                poisoned.into_inner()
            }
        };
        guard.insert(tenant_id, until);
    }

    fn is_armed(&self, tenant_id: Uuid) -> bool {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard
            .get(&tenant_id)
            .copied()
            .is_some_and(|until| Instant::now() < until)
    }
}

// ── Redis-backed implementation ──────────────────────────────────────────

/// Production cache layer. Uses a `redis::aio::ConnectionManager` (auto-reconnect)
/// so transient Redis blips don't cascade into request errors.
pub struct RedisCatalogCache {
    conn: redis::aio::ConnectionManager,
    skip_until: SkipUntilMap,
}

impl RedisCatalogCache {
    /// Connect with `redis::aio::ConnectionManager` so reconnects are handled
    /// transparently. Caller passes a `redis://...` URL from `cfg.redis_url`.
    ///
    /// # Errors
    ///
    /// Returns an error if the URL is malformed or the initial connection
    /// fails. Callers that get an error should fall back to [`NoopCatalogCache`]
    /// rather than refusing to boot — single-replica dev installs need to
    /// keep working without Redis.
    pub async fn connect(redis_url: &str) -> anyhow::Result<Self> {
        let client = redis::Client::open(redis_url)?;
        let conn = client.get_connection_manager().await?;
        Ok(Self {
            conn,
            skip_until: SkipUntilMap::default(),
        })
    }

    /// `SCAN MATCH pattern + UNLINK` — never `KEYS`, never `FLUSHDB`.
    /// `UNLINK` is preferred over `DEL` because it is asynchronous on the
    /// server side and won't block large invalidations.
    async fn scan_and_unlink(&self, pattern: &str) -> anyhow::Result<()> {
        // Take a fresh handle to the connection manager so the `&mut` borrow
        // doesn't escape this method. ConnectionManager is `Clone` and
        // multiplexes across underlying connections.
        let mut conn = self.conn.clone();
        let mut cursor: u64 = 0;
        loop {
            let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await?;
            if !batch.is_empty() {
                let _: i64 = redis::cmd("UNLINK")
                    .arg(&batch)
                    .query_async(&mut conn)
                    .await?;
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl CatalogCache for RedisCatalogCache {
    async fn get(
        &self,
        key: &str,
        requesting_tenant_id: Uuid,
    ) -> anyhow::Result<Option<CatalogResponse>> {
        let mut conn = self.conn.clone();
        let raw: Option<Vec<u8>> = conn.get(key).await?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        match serde_json::from_slice::<CatalogResponse>(&raw) {
            Ok(payload) => {
                if payload.tenant_id == requesting_tenant_id {
                    Ok(Some(payload))
                } else {
                    // Defense in depth — the key already encodes tenant; a
                    // mismatch on the embedded `tenant_id` means either a
                    // backend misconfig or an attacker-controlled collision.
                    // Drop the entry, log, and serve a miss so the resolver
                    // repopulates authoritative state.
                    tracing::warn!(
                        cache_key = %key,
                        embedded_tenant = %payload.tenant_id,
                        requesting_tenant = %requesting_tenant_id,
                        "catalog_cache: tenant_id mismatch on hydrate; \
                         dropping cached entry and forcing miss"
                    );
                    let _: Result<i64, _> = conn.unlink::<_, i64>(key).await;
                    Ok(None)
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    cache_key = %key,
                    "catalog_cache: decode failed; forcing miss"
                );
                let _: Result<i64, _> = conn.unlink::<_, i64>(key).await;
                Ok(None)
            }
        }
    }

    async fn put(&self, key: &str, payload: &CatalogResponse, ttl: Duration) -> anyhow::Result<()> {
        let mut conn = self.conn.clone();
        let bytes = serde_json::to_vec(payload)?;
        // `set_ex` issues `SET ... EX <seconds>` — atomic write + TTL.
        let secs: u64 = ttl.as_secs();
        let _: () = conn.set_ex(key, bytes, secs).await?;
        Ok(())
    }

    async fn invalidate(&self, tenant_id: Uuid, mode: InvalidateMode) -> anyhow::Result<()> {
        let pattern = format!("{CACHE_KEY_PREFIX}{}:*", tenant_id.hyphenated());
        self.scan_and_unlink(&pattern).await?;
        if mode == InvalidateMode::Lock {
            self.skip_until
                .arm(tenant_id, Instant::now() + LOCK_BYPASS_WINDOW);
        }
        Ok(())
    }

    async fn flush_all(&self) -> anyhow::Result<()> {
        // `cat:v1:*` — NEVER `FLUSHDB`. The Redis instance is shared with
        // sibling namespaces and a global flush would clobber them.
        self.scan_and_unlink(&format!("{CACHE_KEY_PREFIX}*")).await
    }

    fn should_skip(&self, tenant_id: Uuid) -> bool {
        self.skip_until.is_armed(tenant_id)
    }
}

// ── No-op implementation ─────────────────────────────────────────────────

/// No-op cache. Used when `cfg.redis_url` is unset (single-replica dev) and
/// also as the test default — `get` always returns miss, `put`/`invalidate`/
/// `flush_all` are no-ops, and `should_skip` is always `false`.
///
/// The skip-until map is still populated on `invalidate(Lock)` so unit tests
/// can exercise the bypass-window contract without standing up Redis.
#[derive(Default)]
pub struct NoopCatalogCache {
    skip_until: SkipUntilMap,
}

#[async_trait]
impl CatalogCache for NoopCatalogCache {
    async fn get(
        &self,
        _key: &str,
        _requesting_tenant_id: Uuid,
    ) -> anyhow::Result<Option<CatalogResponse>> {
        Ok(None)
    }

    async fn put(
        &self,
        _key: &str,
        _payload: &CatalogResponse,
        _ttl: Duration,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn invalidate(&self, tenant_id: Uuid, mode: InvalidateMode) -> anyhow::Result<()> {
        if mode == InvalidateMode::Lock {
            self.skip_until
                .arm(tenant_id, Instant::now() + LOCK_BYPASS_WINDOW);
        }
        Ok(())
    }

    async fn flush_all(&self) -> anyhow::Result<()> {
        tracing::info!(
            prefix = CACHE_KEY_PREFIX,
            "catalog_cache: flush_all called on no-op stub"
        );
        Ok(())
    }

    fn should_skip(&self, tenant_id: Uuid) -> bool {
        self.skip_until.is_armed(tenant_id)
    }
}

#[cfg(test)]
mod tests {
    //! Pure-unit coverage. Live Redis tests live in `live_tests.rs`.

    use super::*;
    use chrono::Utc;

    const T1: Uuid = Uuid::from_u128(0x1111_1111_1111_1111_1111_1111_1111_1111_u128);
    const T2: Uuid = Uuid::from_u128(0x2222_2222_2222_2222_2222_2222_2222_2222_u128);

    // ── Cache key shape ─────────────────────────────────────────────────

    #[test]
    fn prefix_is_cat_v1_colon() {
        // Cross-component pin: seed flush, admin invalidate, and the resolver
        // all derive their keys from this constant. A typo silently desyncs.
        assert_eq!(CACHE_KEY_PREFIX, "cat:v1:");
    }

    #[test]
    fn key_includes_tenant_role_team_in_canonical_order() {
        let key = cache_key(T1, Some("eng"), Some("alpha"));
        assert_eq!(key, "cat:v1:11111111-1111-1111-1111-111111111111:eng:alpha");
    }

    #[test]
    fn none_role_and_empty_role_produce_same_key() {
        // Determinism gate per DESIGN §3.2: the empty-string sentinel for
        // `role_slug` / `team_id` is the SAME key regardless of whether the
        // caller passed `None` or `Some("")`. A regression here is a
        // silent cache miss on every request whose other side spells it the
        // other way.
        let none_key = cache_key(T1, None, None);
        let empty_key = cache_key(T1, Some(""), Some(""));
        assert_eq!(none_key, empty_key);
        assert_eq!(none_key, "cat:v1:11111111-1111-1111-1111-111111111111::");
    }

    #[test]
    fn none_role_only_collapses_to_empty_segment() {
        let key = cache_key(T1, None, Some("alpha"));
        assert_eq!(key, "cat:v1:11111111-1111-1111-1111-111111111111::alpha");
    }

    #[test]
    fn role_with_colon_is_percent_encoded_cannot_collide_with_team() {
        // Hostile / accidental `role_slug = "a:b"` MUST percent-encode to
        // `a%3Ab` so it cannot bleed into the team-segment position. Without
        // encoding, `role = "a"` + `team = "b"` would produce the SAME key
        // as `role = "a:b"` + `team = ""` — a cross-context collision.
        let a_b_team = cache_key(T1, Some("a"), Some("b"));
        let role_colon = cache_key(T1, Some("a:b"), Some(""));
        assert_ne!(a_b_team, role_colon, "colon in role MUST encode");
        assert_eq!(
            role_colon,
            "cat:v1:11111111-1111-1111-1111-111111111111:a%3Ab:"
        );
    }

    #[test]
    fn role_with_percent_is_percent_encoded() {
        // `%` itself is not in the whitelist — must encode to `%25` to avoid
        // ambiguity with an already-encoded segment.
        let key = cache_key(T1, Some("100%"), Some(""));
        assert_eq!(key, "cat:v1:11111111-1111-1111-1111-111111111111:100%25:");
    }

    #[test]
    fn allowed_chars_pass_through_unchanged() {
        // The whitelist `[A-Za-z0-9_\-]` must round-trip cleanly so the
        // common case (alphanumeric role/team identifiers) doesn't pay
        // encoding overhead in the hot path.
        let key = cache_key(T1, Some("eng_lead-1"), Some("Team_42"));
        assert_eq!(
            key,
            "cat:v1:11111111-1111-1111-1111-111111111111:eng_lead-1:Team_42"
        );
    }

    #[test]
    fn url_safe_encode_uppercase_hex() {
        // Pin the hex case — mixing cases across call sites would create
        // silent cache misses where the encoder on the read path and the
        // write path disagree. RFC 3986 §2.1 recommends uppercase.
        assert_eq!(url_safe_encode(":"), "%3A");
        assert_eq!(url_safe_encode("/"), "%2F");
    }

    #[test]
    fn url_safe_encode_high_bytes() {
        // Non-ASCII bytes (UTF-8 multi-byte sequences) must each encode.
        // `ñ` = 0xC3 0xB1.
        assert_eq!(url_safe_encode("ñ"), "%C3%B1");
    }

    // ── Skip-until map (lock-bypass window) ─────────────────────────────

    #[tokio::test]
    async fn noop_invalidate_lock_arms_skip_window() {
        let cache = NoopCatalogCache::default();
        assert!(!cache.should_skip(T1));
        cache
            .invalidate(T1, InvalidateMode::Lock)
            .await
            .unwrap_or_else(|e| panic!("noop invalidate must succeed: {e}"));
        assert!(
            cache.should_skip(T1),
            "lock invalidate MUST arm skip window"
        );
        // Other tenants are not affected by T1's window.
        assert!(!cache.should_skip(T2));
    }

    #[tokio::test]
    async fn noop_invalidate_standard_does_not_arm_skip_window() {
        let cache = NoopCatalogCache::default();
        cache
            .invalidate(T1, InvalidateMode::Standard)
            .await
            .unwrap_or_else(|e| panic!("noop invalidate must succeed: {e}"));
        assert!(
            !cache.should_skip(T1),
            "standard invalidate MUST NOT arm the skip window — \
             the window is reserved for compliance-critical lock writes"
        );
    }

    #[tokio::test]
    async fn noop_get_always_misses_and_put_is_noop() {
        let cache = NoopCatalogCache::default();
        let miss = cache
            .get("cat:v1:any", T1)
            .await
            .unwrap_or_else(|e| panic!("noop get must succeed: {e}"));
        assert!(miss.is_none(), "no-op cache MUST always miss on get");

        let payload = CatalogResponse {
            tenant_id: T1,
            generated_at: Utc::now(),
            metrics: vec![],
        };
        cache
            .put("cat:v1:any", &payload, DEFAULT_TTL)
            .await
            .unwrap_or_else(|e| panic!("noop put must succeed: {e}"));
        // Still misses after put — that's the contract.
        let still_miss = cache
            .get("cat:v1:any", T1)
            .await
            .unwrap_or_else(|e| panic!("noop get must succeed: {e}"));
        assert!(still_miss.is_none());
    }

    #[tokio::test]
    async fn noop_flush_is_ok() {
        let cache = NoopCatalogCache::default();
        cache
            .flush_all()
            .await
            .unwrap_or_else(|e| panic!("no-op must never error: {e}"));
    }

    // The 5 s window decay (`should_skip(T1) == false` after sleeping past
    // LOCK_BYPASS_WINDOW) is exercised by the live-Redis tests rather than
    // here so unit tests don't pay 5 s of wall-clock per `cargo test` run.
    // Coverage of the time-bounded property still lives in the test suite —
    // see `infra/cache/live_tests.rs::lock_bypass_window_expires`.
}
