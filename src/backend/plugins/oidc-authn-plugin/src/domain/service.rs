//! OIDC token validation service.
//!
//! Uses `modkit-auth` `JwksKeyProvider` for JWT signature validation
//! and `validate_claims` for standard claim checks.

use modkit_auth::{JwksKeyProvider, ValidationConfig, validate_claims};
use modkit_auth::traits::KeyProvider;
use modkit_security::SecurityContext;
use secrecy::SecretString;
use std::sync::Arc;
use uuid::Uuid;

use crate::config::OidcAuthnPluginConfig;

/// Errors from OIDC token validation.
#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("token signature validation failed: {0}")]
    SignatureInvalid(String),

    #[error("token claims validation failed: {0}")]
    ClaimsInvalid(String),

    #[error("missing required claim: {0}")]
    MissingClaim(String),

    #[error("invalid claim format: {field} — {reason}")]
    InvalidClaimFormat { field: String, reason: String },
}

/// OIDC token validation service.
///
/// Validates JWT bearer tokens using JWKS key discovery and
/// builds a `SecurityContext` from the validated claims.
pub struct OidcService {
    key_provider: Arc<JwksKeyProvider>,
    validation_config: ValidationConfig,
    tenant_claim: String,
    subject_type: String,
}

impl OidcService {
    /// Creates a new OIDC service from plugin configuration.
    ///
    /// # Errors
    ///
    /// Returns error if the JWKS key provider cannot be created.
    #[must_use]
    pub fn new(
        config: &OidcAuthnPluginConfig,
        key_provider: Arc<JwksKeyProvider>,
    ) -> Self {
        let mut allowed_issuers = Vec::new();
        if !config.issuer_url.is_empty() {
            allowed_issuers.push(config.issuer_url.clone());
        }

        let mut allowed_audiences = Vec::new();
        if !config.audience.is_empty() {
            allowed_audiences.push(config.audience.clone());
        }

        let validation_config = ValidationConfig {
            allowed_issuers,
            allowed_audiences,
            leeway_seconds: config.leeway_seconds,
            require_exp: true,
        };

        Self {
            key_provider,
            validation_config,
            tenant_claim: config.tenant_claim.clone(),
            subject_type: config.subject_type.clone(),
        }
    }

    /// Validates a JWT bearer token and returns a `SecurityContext`.
    ///
    /// # Flow
    ///
    /// 1. Validate JWT signature using JWKS keys
    /// 2. Validate standard claims (iss, aud, exp, nbf)
    /// 3. Extract subject (`sub` claim) → `subject_id`
    /// 4. Extract tenant (`tenant_claim`) → `subject_tenant_id`
    /// 5. Extract scopes (`scp` or `scope` claim) → `token_scopes`
    /// 6. Build `SecurityContext`
    ///
    /// # Errors
    ///
    /// Returns `OidcError` if token is invalid, expired, or missing required claims.
    pub async fn validate_token(&self, token: &str) -> Result<SecurityContext, OidcError> {
        // 1. Validate signature and decode claims
        let (_header, claims) = self
            .key_provider
            .validate_and_decode(token)
            .await
            .map_err(|e| OidcError::SignatureInvalid(e.to_string()))?;

        // 2. Validate standard claims
        validate_claims(&claims, &self.validation_config)
            .map_err(|e| OidcError::ClaimsInvalid(e.to_string()))?;

        // 3. Extract subject_id from `sub` claim
        let sub_str = claims
            .get("sub")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| OidcError::MissingClaim("sub".to_owned()))?;

        // OIDC `sub` is often not a UUID — use a deterministic UUID v5 from the issuer+sub
        let subject_id = uuid_from_sub(sub_str);

        // 4. Extract tenant_id from configured claim
        let subject_tenant_id = claims
            .get(&self.tenant_claim)
            .and_then(serde_json::Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_default();

        // 5. Extract scopes from `scp` (Okta) or `scope` (standard) claim
        let token_scopes = extract_scopes(&claims);

        // 6. Build SecurityContext
        let ctx = SecurityContext::builder()
            .subject_id(subject_id)
            .subject_type(&self.subject_type)
            .subject_tenant_id(subject_tenant_id)
            .token_scopes(token_scopes)
            .bearer_token(SecretString::from(token.to_owned()))
            .build()
            .map_err(|e| OidcError::InvalidClaimFormat {
                field: "security_context".to_owned(),
                reason: e.to_string(),
            })?;

        Ok(ctx)
    }
}

/// Creates a deterministic UUID v5 from an OIDC `sub` claim.
/// Uses the URL namespace so the same sub always maps to the same UUID.
fn uuid_from_sub(sub: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, sub.as_bytes())
}

/// Extracts scopes from JWT claims.
/// Supports Okta-style `scp` (array) and standard `scope` (space-delimited string).
fn extract_scopes(claims: &serde_json::Value) -> Vec<String> {
    // Try `scp` first (Okta style — array of strings)
    if let Some(scp) = claims.get("scp").and_then(serde_json::Value::as_array) {
        return scp
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(String::from)
            .collect();
    }

    // Try `scope` (standard — space-delimited string)
    if let Some(scope) = claims.get("scope").and_then(serde_json::Value::as_str) {
        return scope.split_whitespace().map(String::from).collect();
    }

    // No scopes — return wildcard (unrestricted)
    vec!["*".to_owned()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_from_sub_is_deterministic() {
        let id1 = uuid_from_sub("auth0|user123");
        let id2 = uuid_from_sub("auth0|user123");
        assert_eq!(id1, id2);
    }

    #[test]
    fn uuid_from_sub_different_subs_differ() {
        let id1 = uuid_from_sub("auth0|user123");
        let id2 = uuid_from_sub("auth0|user456");
        assert_ne!(id1, id2);
    }

    #[test]
    fn extract_scopes_okta_style() {
        let claims = serde_json::json!({
            "scp": ["openid", "profile", "email"]
        });
        let scopes = extract_scopes(&claims);
        assert_eq!(scopes, vec!["openid", "profile", "email"]);
    }

    #[test]
    fn extract_scopes_standard_style() {
        let claims = serde_json::json!({
            "scope": "openid profile email"
        });
        let scopes = extract_scopes(&claims);
        assert_eq!(scopes, vec!["openid", "profile", "email"]);
    }

    #[test]
    fn extract_scopes_none_returns_wildcard() {
        let claims = serde_json::json!({});
        let scopes = extract_scopes(&claims);
        assert_eq!(scopes, vec!["*"]);
    }

    #[test]
    fn extract_scopes_scp_takes_priority() {
        let claims = serde_json::json!({
            "scp": ["admin"],
            "scope": "read write"
        });
        let scopes = extract_scopes(&claims);
        assert_eq!(scopes, vec!["admin"]);
    }
}
