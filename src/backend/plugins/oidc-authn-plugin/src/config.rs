//! Configuration for the OIDC `AuthN` plugin.

use serde::Deserialize;

/// OIDC plugin configuration.
///
/// # Example (YAML)
///
/// ```yaml
/// modules:
///   oidc-authn-plugin:
///     config:
///       vendor: "hyperspot"
///       priority: 50
///       issuer_url: "https://dev-12345.okta.com/oauth2/default"
///       audience: "api://insight"
///       jwks_refresh_interval_seconds: 300
///       tenant_claim: "tenant_id"
///       subject_type: "user"
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OidcAuthnPluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,

    /// OIDC issuer URL (e.g., `https://dev-12345.okta.com/oauth2/default`).
    /// Used to construct the JWKS URI: `{issuer_url}/.well-known/openid-configuration`
    /// or directly `{issuer_url}/v1/keys` for Okta.
    pub issuer_url: String,

    /// Expected audience claim (`aud`). If empty, audience is not validated.
    pub audience: String,

    /// JWKS endpoint URL override. If empty, derived from `issuer_url`.
    pub jwks_url: String,

    /// JWKS key refresh interval in seconds.
    pub jwks_refresh_interval_seconds: u64,

    /// JWT claim name containing the tenant ID.
    /// Okta custom claims or standard claims can be used.
    /// If the claim is missing, the subject's home tenant defaults to a nil UUID.
    pub tenant_claim: String,

    /// Subject type passed to `SecurityContext` (e.g., "user", "service").
    pub subject_type: String,

    /// Leeway in seconds for token expiry validation.
    pub leeway_seconds: i64,
}

impl Default for OidcAuthnPluginConfig {
    fn default() -> Self {
        Self {
            vendor: "hyperspot".to_owned(),
            priority: 50,
            issuer_url: String::new(),
            audience: String::new(),
            jwks_url: String::new(),
            jwks_refresh_interval_seconds: 300,
            tenant_claim: "tenant_id".to_owned(),
            subject_type: "user".to_owned(),
            leeway_seconds: 60,
        }
    }
}

impl OidcAuthnPluginConfig {
    /// Returns the effective JWKS URL.
    /// If `jwks_url` is set, use it. Otherwise derive from `issuer_url`.
    #[must_use]
    pub fn effective_jwks_url(&self) -> String {
        if self.jwks_url.is_empty() {
            format!("{}/v1/keys", self.issuer_url.trim_end_matches('/'))
        } else {
            self.jwks_url.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let cfg = OidcAuthnPluginConfig::default();
        assert_eq!(cfg.vendor, "hyperspot");
        assert_eq!(cfg.priority, 50);
        assert_eq!(cfg.tenant_claim, "tenant_id");
        assert_eq!(cfg.leeway_seconds, 60);
    }

    #[test]
    fn jwks_url_derived_from_issuer() {
        let cfg = OidcAuthnPluginConfig {
            issuer_url: "https://dev-123.okta.com/oauth2/default".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_jwks_url(),
            "https://dev-123.okta.com/oauth2/default/v1/keys"
        );
    }

    #[test]
    fn jwks_url_override() {
        let cfg = OidcAuthnPluginConfig {
            issuer_url: "https://okta.example.com".to_owned(),
            jwks_url: "https://custom.example.com/.well-known/jwks.json".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_jwks_url(),
            "https://custom.example.com/.well-known/jwks.json"
        );
    }

    #[test]
    fn issuer_trailing_slash_stripped() {
        let cfg = OidcAuthnPluginConfig {
            issuer_url: "https://okta.example.com/".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_jwks_url(),
            "https://okta.example.com/v1/keys"
        );
    }
}
