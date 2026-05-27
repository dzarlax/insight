//! Live Redis integration tests for the catalog cache layer (Refs #524).
//!
//! All tests are `#[ignore]`d by default and skip silently when `REDIS_URL`
//! is unset. Set `REDIS_URL=redis://127.0.0.1:6379` against a throwaway Redis
//! to exercise them.
//!
//! Coverage map vs the issue's Definition of Done:
//! - `DoD` #6 (≤ 2 s p99 cross-replica invalidation) — exercised by
//!   [`cross_instance_invalidation_is_visible_immediately`]. Two separate
//!   `RedisCatalogCache` instances against the SAME Redis URL stand in for
//!   two analytics-api replicas: replica A invalidates → replica B's `get`
//!   sees the absence immediately. (The "2 s" budget is a Redis-side
//!   property — Redis is the shared backend; the test exercises the
//!   contract that the cache is in fact shared, not the network latency.)
//! - `DoD` #7 (cross-tenant cache hydrate mismatch → miss + warning log) —
//!   [`cross_tenant_hydrate_forces_miss`].
//! - `DoD` lock-bypass 5 s window — [`lock_bypass_window_expires`]. This is
//!   the test that pays wall-clock seconds (sleeps past `LOCK_BYPASS_WINDOW`),
//!   so it lives behind the env-var gate.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use crate::domain::catalog::response::{CatalogResponse, MetricView, ThresholdView};
use crate::infra::cache::catalog_cache::{
    CACHE_KEY_PREFIX, CatalogCache, InvalidateMode, LOCK_BYPASS_WINDOW, RedisCatalogCache,
    cache_key,
};

const ENV_VAR: &str = "REDIS_URL";

async fn connect_or_skip() -> Option<RedisCatalogCache> {
    let Ok(url) = std::env::var(ENV_VAR) else {
        eprintln!("skipping: {ENV_VAR} not set");
        return None;
    };
    match RedisCatalogCache::connect(&url).await {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipping: cannot connect to {ENV_VAR}: {e}");
            None
        }
    }
}

fn sample_payload(tenant_id: Uuid) -> CatalogResponse {
    CatalogResponse {
        tenant_id,
        generated_at: Utc::now(),
        metrics: vec![MetricView {
            id: Uuid::now_v7(),
            label: "Tasks Closed".to_owned(),
            sublabel: None,
            description: None,
            unit: None,
            format: Some("integer".to_owned()),
            higher_is_better: true,
            is_member_scale: false,
            source_tags: vec!["jira".to_owned()],
            schema_status: "ok".to_owned(),
            schema_error_code: None,
            thresholds: ThresholdView {
                good: 5.0,
                warn: 3.0,
                alert_trigger: None,
                alert_bad: None,
                resolved_from: "product-default".to_owned(),
                bounded_by_lock: false,
            },
        }],
    }
}

#[tokio::test]
#[ignore = "requires live Redis; set REDIS_URL to enable"]
async fn put_then_get_roundtrips() -> anyhow::Result<()> {
    let Some(cache) = connect_or_skip().await else {
        return Ok(());
    };

    let tenant = Uuid::now_v7();
    let key = cache_key(tenant, Some("eng"), Some("alpha"));
    let payload = sample_payload(tenant);

    cache.put(&key, &payload, Duration::from_mins(1)).await?;
    let got = cache.get(&key, tenant).await?;
    assert!(got.is_some(), "round-trip MUST return Some");
    let got = got.unwrap_or_else(|| panic!("just asserted Some"));
    assert_eq!(got.tenant_id, tenant);

    cache.invalidate(tenant, InvalidateMode::Standard).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires live Redis; set REDIS_URL to enable"]
async fn cross_tenant_hydrate_forces_miss() -> anyhow::Result<()> {
    // `DoD` #7: a cached payload whose embedded `tenant_id` is T2 MUST NOT be
    // served to a T1 request, even if the cache key collided (which would
    // only happen via misconfig). The cache returns None + logs a security
    // warning + unlinks the offending entry.
    let Some(cache) = connect_or_skip().await else {
        return Ok(());
    };

    let t1 = Uuid::now_v7();
    let t2 = Uuid::now_v7();
    let key = cache_key(t1, None, None); // T1 key shape...
    let mismatched = sample_payload(t2); // ...but the payload is for T2.

    cache.put(&key, &mismatched, Duration::from_mins(1)).await?;
    let got = cache.get(&key, t1).await?;
    assert!(
        got.is_none(),
        "cross-tenant cached payload MUST be served as miss, not as a hit"
    );

    // The mismatched entry MUST have been unlinked from Redis — a re-read
    // confirms the entry is gone.
    let again = cache.get(&key, t2).await?;
    assert!(
        again.is_none(),
        "mismatched entry MUST be unlinked from Redis (defense in depth)"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires live Redis; set REDIS_URL to enable"]
async fn invalidate_tenant_prefix_does_not_clobber_sibling_tenants() -> anyhow::Result<()> {
    // Critical invariant: tenant-prefix purge MUST be tenant-scoped. A
    // sloppy implementation that did `FLUSHDB` or used a wildcard match
    // wider than `cat:v1:{tenant_id}:*` would clobber sibling tenants.
    let Some(cache) = connect_or_skip().await else {
        return Ok(());
    };

    let t1 = Uuid::now_v7();
    let t2 = Uuid::now_v7();
    let k1 = cache_key(t1, Some("eng"), None);
    let k2 = cache_key(t2, Some("eng"), None);

    cache
        .put(&k1, &sample_payload(t1), Duration::from_mins(1))
        .await?;
    cache
        .put(&k2, &sample_payload(t2), Duration::from_mins(1))
        .await?;

    cache.invalidate(t1, InvalidateMode::Standard).await?;

    assert!(
        cache.get(&k1, t1).await?.is_none(),
        "T1 entry MUST be purged"
    );
    assert!(
        cache.get(&k2, t2).await?.is_some(),
        "T2 entry MUST survive T1's invalidation"
    );

    cache.invalidate(t2, InvalidateMode::Standard).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires live Redis; set REDIS_URL to enable; sleeps past LOCK_BYPASS_WINDOW"]
async fn lock_bypass_window_expires() -> anyhow::Result<()> {
    // The lock-bypass window is `2 ×` the cross-replica invalidation p99
    // (= 5 s). `should_skip(tenant)` MUST return true right after
    // `invalidate(Lock)` and MUST return false after the window elapses.
    //
    // This test sleeps `LOCK_BYPASS_WINDOW + 200ms`, so it's gated behind
    // the live-Redis env var to keep `cargo test` fast on stock dev.
    let Some(cache) = connect_or_skip().await else {
        return Ok(());
    };

    let tenant = Uuid::now_v7();
    assert!(!cache.should_skip(tenant), "fresh tenant must not be armed");

    cache.invalidate(tenant, InvalidateMode::Lock).await?;
    assert!(
        cache.should_skip(tenant),
        "should_skip MUST be true immediately after invalidate(Lock)"
    );

    tokio::time::sleep(LOCK_BYPASS_WINDOW + Duration::from_millis(200)).await;
    assert!(
        !cache.should_skip(tenant),
        "should_skip MUST decay to false after LOCK_BYPASS_WINDOW"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires live Redis; set REDIS_URL to enable"]
async fn cross_instance_invalidation_is_visible_immediately() -> anyhow::Result<()> {
    // `DoD` #6: two `RedisCatalogCache` instances against the SAME Redis URL
    // (= two analytics-api replicas in prod) MUST see each other's
    // invalidations. Replica A invalidates → replica B's `get` returns None
    // within the read.
    let Ok(url) = std::env::var(ENV_VAR) else {
        eprintln!("skipping: {ENV_VAR} not set");
        return Ok(());
    };
    let a = Arc::new(RedisCatalogCache::connect(&url).await?);
    let b = Arc::new(RedisCatalogCache::connect(&url).await?);

    let tenant = Uuid::now_v7();
    let key = cache_key(tenant, Some("eng"), Some("alpha"));
    let payload = sample_payload(tenant);

    a.put(&key, &payload, Duration::from_mins(1)).await?;
    assert!(
        b.get(&key, tenant).await?.is_some(),
        "instance B MUST see instance A's write through the shared Redis"
    );

    a.invalidate(tenant, InvalidateMode::Standard).await?;
    assert!(
        b.get(&key, tenant).await?.is_none(),
        "instance B MUST see instance A's invalidation through the shared Redis"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires live Redis; set REDIS_URL to enable"]
async fn flush_all_uses_cat_v1_prefix_not_flushdb() -> anyhow::Result<()> {
    // Critical isolation property: `flush_all` MUST be a `cat:v1:*` prefix
    // purge — not a global FLUSHDB. Verified by writing a key OUTSIDE the
    // catalog prefix and asserting it survives `flush_all`.
    let Ok(url) = std::env::var(ENV_VAR) else {
        eprintln!("skipping: {ENV_VAR} not set");
        return Ok(());
    };
    let cache = RedisCatalogCache::connect(&url).await?;

    // Write a sibling-namespace key directly (simulating
    // Identity Resolution's `person_aliases:*`).
    let client = redis::Client::open(url.clone())?;
    let mut conn = client.get_multiplexed_async_connection().await?;
    let sibling_key = format!("person_aliases:cache_test:{}", Uuid::now_v7().hyphenated());
    let _: () = redis::cmd("SET")
        .arg(&sibling_key)
        .arg("survivor")
        .query_async(&mut conn)
        .await?;

    // Write a catalog entry under the cache prefix.
    let tenant = Uuid::now_v7();
    let key = cache_key(tenant, None, None);
    cache
        .put(&key, &sample_payload(tenant), Duration::from_mins(1))
        .await?;
    assert!(
        key.starts_with(CACHE_KEY_PREFIX),
        "key under test must be in the catalog namespace"
    );

    cache.flush_all().await?;

    // Catalog key gone, sibling key intact.
    assert!(cache.get(&key, tenant).await?.is_none());
    let survivor: Option<String> = redis::cmd("GET")
        .arg(&sibling_key)
        .query_async(&mut conn)
        .await?;
    assert_eq!(
        survivor.as_deref(),
        Some("survivor"),
        "flush_all MUST NOT touch keys outside cat:v1:*; a global FLUSHDB \
         would have wiped this sibling-namespace key"
    );

    // Clean up.
    let _: () = redis::cmd("DEL")
        .arg(&sibling_key)
        .query_async(&mut conn)
        .await?;
    Ok(())
}
