//! Identity Resolution API client.
//!
//! Resolves Insight person IDs to source-specific aliases.
//! Used when querying Silver tables that don't have a unified `person_id`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A resolved alias from Identity Resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonAlias {
    pub alias_type: String,
    pub alias_value: String,
    pub insight_source_id: Uuid,
}

#[derive(Deserialize)]
struct AliasResponse {
    aliases: Vec<PersonAlias>,
}

/// RFC 9457 Problem Details (subset for error parsing).
#[derive(Deserialize)]
struct ProblemDetails {
    #[allow(dead_code)]
    r#type: Option<String>,
    #[allow(dead_code)]
    title: Option<String>,
    detail: Option<String>,
}

/// Identity Resolution API client.
pub struct IdentityResolutionClient {
    base_url: String,
    http: reqwest::Client,
}

impl IdentityResolutionClient {
    #[must_use]
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            http: reqwest::Client::new(),
        }
    }

    /// Resolve a person ID to all known aliases.
    ///
    /// Calls `GET {base_url}/v1/persons/{person_id}/aliases` with the Bearer token
    /// forwarded from the original request.
    ///
    /// # Errors
    ///
    /// Returns error if the Identity Resolution API is unreachable or returns an error.
    pub async fn resolve_aliases(
        &self,
        person_id: Uuid,
        bearer_token: &str,
    ) -> anyhow::Result<Vec<PersonAlias>> {
        let url = format!("{}/v1/persons/{person_id}/aliases", self.base_url);

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {bearer_token}"))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            // Parse upstream RFC 9457 Problem Details if available
            let detail = resp
                .json::<ProblemDetails>()
                .await
                .map(|p| p.detail.unwrap_or_default())
                .unwrap_or_default();
            tracing::warn!(
                person_id = %person_id,
                status = %status,
                detail = %detail,
                "identity resolution request failed"
            );
            anyhow::bail!("identity resolution returned {status}: {detail}");
        }

        let data: AliasResponse = resp.json().await?;
        Ok(data.aliases)
    }
}
