//! `POST /catalog/get_metrics` HTTP handler (Refs #524).
//!
//! Implements `cpt-metric-cat-component-catalog-reader`'s HTTP surface per
//! DESIGN §3.3 "Catalog Read":
//!
//! - **Auth**: bearer-token-only at the gateway (out of scope here — Q1 ack);
//!   the request-context fields `role_slug` / `team_id` are accepted from the
//!   JSON body only. `tenant_id` is NEVER taken from the body; it is resolved
//!   server-side by `tenant_middleware` (Refs #522) which has already populated
//!   `SecurityContext.insight_tenant_id` by the time we run.
//! - **Content-Type**: `application/json` required; other types → 415
//!   (`failed_precondition`-shape rejection in canonical envelope form). Closes
//!   the cross-site form-post CSRF path per DESIGN §3.3.
//! - **Body shape**: `GetMetricsRequest` with `deny_unknown_fields`; a hostile
//!   `tenant_id` smuggled into the body is a 400 `invalid_argument` here.
//!
//! ## Why a custom-extractor pattern instead of `Json(req)`
//!
//! Axum's built-in `Json` extractor rejects on parse failure with its own
//! response shape; we need the canonical RFC 9457 envelope. The handler takes
//! `HeaderMap` + `Bytes`, asserts the content-type itself, then deserializes
//! through `serde_json` so we map every error to a canonical category. The
//! tenant-resolution short-circuit already lives in `auth::tenant_middleware`,
//! so the handler can trust `SecurityContext` to be populated.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use modkit_canonical_errors::{CanonicalError, Problem};

use super::AppState;
use super::error::MetricCatalogError;
use crate::auth::SecurityContext;
use crate::domain::catalog::response::GetMetricsRequest;

/// `POST /catalog/get_metrics` handler.
///
/// # Errors
///
/// - `400 invalid_argument` — malformed body, unknown body fields (incl.
///   `tenant_id`).
/// - `415 unsupported_media_type` — body present without
///   `Content-Type: application/json`.
/// - `500 internal` — resolver / DB failure (Redis blips are absorbed by the
///   reader's degrade-gracefully behavior).
pub async fn get_metrics(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(resp) = require_json_content_type(&headers, body.is_empty()) {
        return *resp;
    }

    // Empty body is the legitimate "no role_slug / no team_id" shape and
    // resolves at the tenant / product-default chain only.
    let req: GetMetricsRequest = if body.is_empty() {
        GetMetricsRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(error = %e, "catalog: request body deserialization failed");
                let err = MetricCatalogError::invalid_argument()
                    .with_field_violation(
                        "body",
                        "request body must be a valid JSON object",
                        "INVALID",
                    )
                    .create();
                return err.into_response();
            }
        }
    };

    let response = match state
        .catalog_reader
        .read(
            ctx.insight_tenant_id,
            req.role_slug.as_deref(),
            req.team_id.as_deref(),
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "catalog: resolver failed");
            let err = CanonicalError::internal("failed to resolve catalog").create();
            return err.into_response();
        }
    };

    axum::Json(response).into_response()
}

/// Enforce `Content-Type: application/json` per DESIGN §3.3 CSRF model.
///
/// Returns `Err(response)` with HTTP 415 (per the issue brief) and a §3.3-shape
/// canonical envelope body. `application/json; charset=utf-8` is accepted (the
/// charset parameter is RFC-2616-compliant and routinely added by browsers and
/// stdlib HTTP clients). An empty body without a Content-Type is treated as
/// `{}` — the legitimate generic-hydrator shape.
fn require_json_content_type(headers: &HeaderMap, body_empty: bool) -> Result<(), Box<Response>> {
    let Some(raw) = headers.get(header::CONTENT_TYPE) else {
        // No Content-Type with an empty body is OK (treated as `{}`).
        // No Content-Type with a non-empty body is rejected to keep the
        // CSRF / shape contract tight.
        if body_empty {
            return Ok(());
        }
        return Err(Box::new(unsupported_media_type_response(
            "Content-Type: application/json required",
        )));
    };
    let Ok(value) = raw.to_str() else {
        return Err(Box::new(unsupported_media_type_response(
            "Content-Type header must be valid ASCII",
        )));
    };

    // Compare the media-type, allowing optional `; charset=…` parameters.
    let mime = value.split(';').next().unwrap_or("").trim();
    if mime.eq_ignore_ascii_case("application/json") {
        Ok(())
    } else {
        Err(Box::new(unsupported_media_type_response(
            "Content-Type: application/json required",
        )))
    }
}

/// Build the 415 response.
///
/// `modkit-canonical-errors` doesn't expose an `unsupported_media_type`
/// category (the §3.3 envelope table closest match is `failed_precondition`,
/// which the crate maps to 400). We start from a `failed_precondition`
/// envelope so the body shape matches the rest of the catalog API, then
/// override the HTTP status to 415 — the value the issue brief explicitly
/// requires for the CSRF closure path. The `gts_type` still reads as
/// `failed_precondition`; consumers that branch on `status` get the 415
/// signal, and consumers that branch on `type` get an envelope they already
/// know how to render.
fn unsupported_media_type_response(detail: &'static str) -> Response {
    let err = MetricCatalogError::failed_precondition()
        .with_precondition_violation(
            "content_type",
            "Content-Type",
            "request must use Content-Type: application/json",
        )
        .create();
    let mut problem = Problem::from(err);
    problem.status = StatusCode::UNSUPPORTED_MEDIA_TYPE.as_u16();
    problem.detail.clear();
    problem.detail.push_str(detail);
    problem.into_response()
}

#[cfg(test)]
mod tests {
    //! Wire-shape coverage for the handler. The handler is small (parse +
    //! delegate), so unit-level tests focus on the validation surface
    //! (content-type enforcement, body deny-unknown-fields). End-to-end
    //! through-router coverage with a live MariaDB + Redis is gated on the
    //! `MARIADB_URL` / `REDIS_URL` env vars in the live-test files.

    use axum::http::HeaderValue;

    use super::*;

    fn extract_status(r: Result<(), Box<Response>>) -> u16 {
        match r {
            Ok(()) => 0,
            Err(resp) => resp.status().as_u16(),
        }
    }

    #[test]
    fn require_json_content_type_accepts_application_json() {
        let mut h = HeaderMap::new();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        assert!(require_json_content_type(&h, false).is_ok());
    }

    #[test]
    fn require_json_content_type_accepts_application_json_with_charset() {
        let mut h = HeaderMap::new();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        assert!(require_json_content_type(&h, false).is_ok());
    }

    #[test]
    fn require_json_content_type_accepts_empty_body_without_header() {
        // Generic hydrators that POST `{}` without a Content-Type still
        // resolve cleanly — the body-empty path treats it as the default
        // request.
        let h = HeaderMap::new();
        assert!(require_json_content_type(&h, true).is_ok());
    }

    #[test]
    fn require_json_content_type_rejects_form_urlencoded_with_415() {
        // CSRF closure: a form post MUST NOT reach the handler. The
        // bearer-token-at-gateway model means there's no cookie to ride, but
        // belt-and-suspenders the form-encoding rejection here. The brief
        // explicitly mandates 415 (not 400) for this path per §3.3 CSRF model.
        let mut h = HeaderMap::new();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        assert_eq!(
            extract_status(require_json_content_type(&h, false)),
            415,
            "non-JSON Content-Type MUST return 415, not 400 — DESIGN §3.3 CSRF model"
        );
    }

    #[test]
    fn require_json_content_type_rejects_text_plain_with_415() {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        assert_eq!(extract_status(require_json_content_type(&h, false)), 415);
    }

    #[test]
    fn require_json_content_type_rejects_non_empty_body_without_header_with_415() {
        let h = HeaderMap::new();
        assert_eq!(extract_status(require_json_content_type(&h, false)), 415);
    }

    #[tokio::test]
    async fn unsupported_media_type_response_has_problem_json_content_type()
    -> Result<(), Box<dyn std::error::Error>> {
        use axum::body::to_bytes;
        let resp = unsupported_media_type_response("test detail");
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json"),
            "wire shape MUST be RFC 9457 Problem Details"
        );
        let bytes = to_bytes(resp.into_body(), 16 * 1024).await?;
        let body: serde_json::Value = serde_json::from_slice(&bytes)?;
        // Status field on the envelope mirrors the HTTP status.
        assert_eq!(body["status"], 415);
        // Resource type still carries the metric-catalog GTS namespace so
        // consumers know which surface emitted the error.
        assert_eq!(
            body["context"]["resource_type"],
            "gts.cf.insight.metric_catalog.metric.v1~"
        );
        Ok(())
    }
}
