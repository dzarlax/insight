//! Route handlers.

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, NotSet, QueryFilter, Set};
use std::sync::Arc;
use uuid::Uuid;

use super::AppState;
use crate::auth::SecurityContext;
use crate::domain::metric::{
    CreateMetricRequest, Metric, MetricSummary, TableColumn, UpdateMetricRequest,
};
use crate::domain::query::{PageInfo, QueryRequest, QueryResponse};
use crate::domain::threshold::{
    self, CreateThresholdRequest, Threshold, UpdateThresholdRequest,
};
use crate::infra::db::entities;

// ── Health ──────────────────────────────────────────────────

pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "healthy" }))
}

// ── Metrics CRUD ────────────────────────────────────────────

pub async fn list_metrics(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
) -> Result<impl IntoResponse, StatusCode> {
    let rows = entities::metrics::Entity::find()
        .filter(entities::metrics::Column::InsightTenantId.eq(ctx.insight_tenant_id))
        .filter(entities::metrics::Column::IsEnabled.eq(true))
        .all(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to list metrics");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let items: Vec<MetricSummary> = rows.into_iter().map(model_to_metric_summary).collect();
    Ok(Json(serde_json::json!({ "items": items })))
}

pub async fn get_metric(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let row = entities::metrics::Entity::find_by_id(id)
        .filter(entities::metrics::Column::InsightTenantId.eq(ctx.insight_tenant_id))
        .one(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to get metric");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(model_to_metric(row)))
}

pub async fn create_metric(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
    Json(req): Json<CreateMetricRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let id = Uuid::now_v7();

    let model = entities::metrics::ActiveModel {
        id: Set(id),
        insight_tenant_id: Set(ctx.insight_tenant_id),
        name: Set(req.name),
        description: Set(req.description),
        query_ref: Set(req.query_ref),
        is_enabled: Set(true),
        created_at: NotSet,
        updated_at: NotSet,
    };

    entities::metrics::Entity::insert(model)
        .exec(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create metric");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let row = entities::metrics::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((StatusCode::CREATED, Json(model_to_metric(row))))
}

pub async fn update_metric(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateMetricRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let existing = entities::metrics::Entity::find_by_id(id)
        .filter(entities::metrics::Column::InsightTenantId.eq(ctx.insight_tenant_id))
        .one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut model: entities::metrics::ActiveModel = existing.into();

    if let Some(name) = req.name {
        model.name = Set(name);
    }
    if let Some(desc) = req.description {
        model.description = Set(Some(desc));
    }
    if let Some(query_ref) = req.query_ref {
        model.query_ref = Set(query_ref);
    }
    if let Some(enabled) = req.is_enabled {
        model.is_enabled = Set(enabled);
    }
    model.updated_at = Set(chrono::Utc::now().into());

    let updated = model.update(&state.db).await.map_err(|e| {
        tracing::error!(error = %e, "failed to update metric");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(model_to_metric(updated)))
}

pub async fn delete_metric(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let existing = entities::metrics::Entity::find_by_id(id)
        .filter(entities::metrics::Column::InsightTenantId.eq(ctx.insight_tenant_id))
        .one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut model: entities::metrics::ActiveModel = existing.into();
    model.is_enabled = Set(false);
    model.updated_at = Set(chrono::Utc::now().into());
    model.update(&state.db).await.map_err(|e| {
        tracing::error!(error = %e, "failed to soft-delete metric");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(StatusCode::NO_CONTENT)
}

// ── Query ───────────────────────────────────────────────────

pub async fn query_metric(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<Uuid>,
    Json(req): Json<QueryRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    // 1. Load metric definition
    let metric = entities::metrics::Entity::find_by_id(id)
        .filter(entities::metrics::Column::InsightTenantId.eq(ctx.insight_tenant_id))
        .filter(entities::metrics::Column::IsEnabled.eq(true))
        .one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // 2. Load thresholds for this metric
    let thresholds = entities::thresholds::Entity::find()
        .filter(entities::thresholds::Column::MetricId.eq(id))
        .filter(entities::thresholds::Column::InsightTenantId.eq(ctx.insight_tenant_id))
        .all(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 3. Validate $top
    let top = req.top.min(200).max(1);

    // 4. Build ClickHouse query from query_ref.
    //
    // query_ref is parsed into (select_expr, table, group_by) on read.
    // The engine always builds FROM and WHERE — insight_tenant_id is
    // always injected for tenant isolation. Admins never control WHERE.
    //
    // TODO: Full implementation should also:
    // - Validate org_unit_id from $filter against AccessScope (IDOR prevention)
    // - Resolve person_ids via Identity Resolution API
    // - Parse $select to restrict returned columns
    // - Implement cursor-based pagination (decode $skip → keyset)

    let (select_expr, table, group_by) = parse_query_ref(&metric.query_ref).map_err(|e| {
        tracing::error!(error = %e, query_ref = %metric.query_ref, "invalid query_ref");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut sql = format!("SELECT {select_expr} FROM {table} WHERE insight_tenant_id = ?");
    let mut params: Vec<String> = vec![ctx.insight_tenant_id.to_string()];

    // Parse OData $filter (simplified — production needs a proper OData parser)
    if let Some(ref filter) = req.filter {
        if let Some(date_from) = extract_odata_value(filter, "metric_date", "ge") {
            sql.push_str(" AND metric_date >= ?");
            params.push(date_from);
        }
        if let Some(date_to) = extract_odata_value(filter, "metric_date", "lt") {
            sql.push_str(" AND metric_date < ?");
            params.push(date_to);
        }
    }

    // Apply GROUP BY from parsed query_ref
    if let Some(ref gb) = group_by {
        sql.push_str(&format!(" GROUP BY {gb}"));
    }

    // Apply $orderby — validate against identifier pattern to prevent injection
    if let Some(ref orderby) = req.orderby {
        if !is_valid_orderby(orderby) {
            tracing::warn!(orderby = %orderby, "rejected invalid $orderby");
            return Err(StatusCode::BAD_REQUEST);
        }
        sql.push_str(&format!(" ORDER BY {orderby}"));
    }

    // Apply pagination (fetch top+1 to detect has_next)
    sql.push_str(&format!(" LIMIT {}", top + 1));

    tracing::debug!(sql = %sql, metric_id = %id, "executing metric query");

    // TODO: Execute the query against ClickHouse.
    // For dynamic metrics (columns vary per metric), we need either:
    // - A generic row type that deserializes any column set
    // - Raw query execution returning serde_json::Value rows
    //
    // Placeholder response with debug info.
    let mut items: Vec<serde_json::Value> = vec![serde_json::json!({
        "_debug_sql": sql,
        "_debug_params": params,
        "_note": "query execution not yet implemented — need dynamic row deserialization"
    })];

    // 5. Evaluate thresholds on each result row
    for item in &mut items {
        if let Some(obj) = item.as_object_mut() {
            let mut threshold_results = serde_json::Map::new();
            for t in &thresholds {
                if let Some(val) = obj.get(&t.field_name).and_then(|v| v.as_f64()) {
                    if threshold::threshold_matches(val, &t.operator, t.value) {
                        // Keep highest severity: critical > warning > good
                        let current = threshold_results
                            .get(&t.field_name)
                            .and_then(|v| v.as_str());
                        if should_upgrade_level(current, &t.level) {
                            threshold_results.insert(
                                t.field_name.clone(),
                                serde_json::Value::String(t.level.clone()),
                            );
                        }
                    }
                }
            }
            obj.insert(
                "_thresholds".to_owned(),
                serde_json::Value::Object(threshold_results),
            );
        }
    }

    let response = QueryResponse {
        items,
        page_info: PageInfo {
            has_next: false,
            cursor: None,
        },
    };

    Ok(Json(response))
}

/// Returns true if `new_level` is higher severity than `current`.
fn should_upgrade_level(current: Option<&str>, new_level: &str) -> bool {
    let rank = |l: &str| match l {
        "critical" => 3,
        "warning" => 2,
        "good" => 1,
        _ => 0,
    };
    match current {
        Some(c) => rank(new_level) > rank(c),
        None => true,
    }
}

/// Simplified OData value extractor.
/// Extracts value from patterns like `field_name ge 'value'`.
fn extract_odata_value(filter: &str, field: &str, op: &str) -> Option<String> {
    let pattern = format!("{field} {op} '");
    if let Some(start) = filter.find(&pattern) {
        let rest = &filter[start + pattern.len()..];
        if let Some(end) = rest.find('\'') {
            return Some(rest[..end].to_owned());
        }
    }
    None
}

/// Parse `query_ref` into (select_expr, table, group_by).
///
/// Expects SQL in the form:
///   `SELECT <columns> FROM <table>`
///   `SELECT <columns> FROM <table> GROUP BY <expr>`
///
/// The engine rebuilds the query with `WHERE insight_tenant_id = ?` always
/// injected, so admins cannot bypass tenant isolation.
fn parse_query_ref(query_ref: &str) -> Result<(String, String, Option<String>), String> {
    let upper = query_ref.to_ascii_uppercase();

    // Find SELECT ... FROM boundary
    let from_pos = upper
        .find(" FROM ")
        .ok_or("query_ref must contain SELECT ... FROM ...")?;

    let select_expr = query_ref[..from_pos]
        .trim()
        .strip_prefix_insensitive("SELECT")
        .ok_or("query_ref must start with SELECT")?
        .trim()
        .to_owned();

    if select_expr.is_empty() {
        return Err("SELECT clause is empty".to_owned());
    }

    let after_from = &query_ref[from_pos + 6..]; // skip " FROM "

    // Find optional GROUP BY
    let group_by_pos = upper[from_pos + 6..].find(" GROUP BY ");
    let (table_part, group_by) = match group_by_pos {
        Some(pos) => (
            after_from[..pos].trim(),
            Some(after_from[pos + 10..].trim().to_owned()), // skip " GROUP BY "
        ),
        None => (after_from.trim(), None),
    };

    let table = table_part.to_owned();
    if table.is_empty() {
        return Err("table name is empty".to_owned());
    }

    // Validate table name is a safe identifier (letters, digits, _, .)
    if !table
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return Err(format!("invalid table name: {table}"));
    }

    Ok((select_expr, table, group_by))
}

/// Case-insensitive prefix strip helper.
trait StripPrefixInsensitive {
    fn strip_prefix_insensitive(&self, prefix: &str) -> Option<&str>;
}

impl StripPrefixInsensitive for str {
    fn strip_prefix_insensitive(&self, prefix: &str) -> Option<&str> {
        if self.len() >= prefix.len()
            && self[..prefix.len()].eq_ignore_ascii_case(prefix)
        {
            Some(&self[prefix.len()..])
        } else {
            None
        }
    }
}

/// Validate an OData `$orderby` expression.
/// Accepts: `column_name [asc|desc] [, column_name [asc|desc]]*`
fn is_valid_orderby(orderby: &str) -> bool {
    if orderby.is_empty() {
        return false;
    }
    orderby.split(',').all(|part| {
        let tokens: Vec<&str> = part.trim().split_whitespace().collect();
        match tokens.len() {
            1 => is_valid_ident(tokens[0]),
            2 => {
                is_valid_ident(tokens[0])
                    && matches!(tokens[1].to_ascii_lowercase().as_str(), "asc" | "desc")
            }
            _ => false,
        }
    })
}

/// Validate a column/table identifier (letters, digits, underscores, dots).
fn is_valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        && !s.starts_with('.')
        && !s.ends_with('.')
}

// ── Thresholds CRUD ─────────────────────────────────────────

pub async fn list_thresholds(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(metric_id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    // Verify metric exists and belongs to tenant
    entities::metrics::Entity::find_by_id(metric_id)
        .filter(entities::metrics::Column::InsightTenantId.eq(ctx.insight_tenant_id))
        .one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let rows = entities::thresholds::Entity::find()
        .filter(entities::thresholds::Column::MetricId.eq(metric_id))
        .filter(entities::thresholds::Column::InsightTenantId.eq(ctx.insight_tenant_id))
        .all(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to list thresholds");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let items: Vec<Threshold> = rows.into_iter().map(model_to_threshold).collect();
    Ok(Json(serde_json::json!({ "items": items })))
}

pub async fn create_threshold(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(metric_id): Path<Uuid>,
    Json(req): Json<CreateThresholdRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    // Verify metric exists and belongs to tenant
    entities::metrics::Entity::find_by_id(metric_id)
        .filter(entities::metrics::Column::InsightTenantId.eq(ctx.insight_tenant_id))
        .one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Validate operator and level
    threshold::validate_threshold(&req.operator, &req.level).map_err(|e| {
        tracing::warn!(error = %e, "invalid threshold");
        StatusCode::BAD_REQUEST
    })?;

    let id = Uuid::now_v7();

    let model = entities::thresholds::ActiveModel {
        id: Set(id),
        insight_tenant_id: Set(ctx.insight_tenant_id),
        metric_id: Set(metric_id),
        field_name: Set(req.field_name),
        operator: Set(req.operator),
        value: Set(req.value),
        level: Set(req.level),
        created_at: NotSet,
        updated_at: NotSet,
    };

    entities::thresholds::Entity::insert(model)
        .exec(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create threshold");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let row = entities::thresholds::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((StatusCode::CREATED, Json(model_to_threshold(row))))
}

pub async fn update_threshold(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
    Path((metric_id, tid)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateThresholdRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let existing = entities::thresholds::Entity::find_by_id(tid)
        .filter(entities::thresholds::Column::MetricId.eq(metric_id))
        .filter(entities::thresholds::Column::InsightTenantId.eq(ctx.insight_tenant_id))
        .one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut model: entities::thresholds::ActiveModel = existing.into();

    if let Some(field_name) = req.field_name {
        model.field_name = Set(field_name);
    }
    if let Some(operator) = req.operator {
        if !threshold::VALID_OPERATORS.contains(&operator.as_str()) {
            return Err(StatusCode::BAD_REQUEST);
        }
        model.operator = Set(operator);
    }
    if let Some(value) = req.value {
        model.value = Set(value);
    }
    if let Some(level) = req.level {
        if !threshold::VALID_LEVELS.contains(&level.as_str()) {
            return Err(StatusCode::BAD_REQUEST);
        }
        model.level = Set(level);
    }
    model.updated_at = Set(chrono::Utc::now().into());

    let updated = model.update(&state.db).await.map_err(|e| {
        tracing::error!(error = %e, "failed to update threshold");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(model_to_threshold(updated)))
}

pub async fn delete_threshold(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
    Path((metric_id, tid)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, StatusCode> {
    let existing = entities::thresholds::Entity::find_by_id(tid)
        .filter(entities::thresholds::Column::MetricId.eq(metric_id))
        .filter(entities::thresholds::Column::InsightTenantId.eq(ctx.insight_tenant_id))
        .one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    entities::thresholds::Entity::delete_by_id(existing.id)
        .exec(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to delete threshold");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(StatusCode::NO_CONTENT)
}

// ── Columns ─────────────────────────────────────────────────

pub async fn list_columns(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
) -> Result<impl IntoResponse, StatusCode> {
    let columns = entities::table_columns::Entity::find()
        .filter(
            Condition::any()
                .add(entities::table_columns::Column::InsightTenantId.is_null())
                .add(entities::table_columns::Column::InsightTenantId.eq(ctx.insight_tenant_id)),
        )
        .all(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to list columns");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let items: Vec<TableColumn> = columns.into_iter().map(model_to_column).collect();
    Ok(Json(serde_json::json!({ "items": items })))
}

pub async fn list_columns_for_table(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(table): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let columns = entities::table_columns::Entity::find()
        .filter(entities::table_columns::Column::ClickhouseTable.eq(&table))
        .filter(
            Condition::any()
                .add(entities::table_columns::Column::InsightTenantId.is_null())
                .add(entities::table_columns::Column::InsightTenantId.eq(ctx.insight_tenant_id)),
        )
        .all(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to list columns for table");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let items: Vec<TableColumn> = columns.into_iter().map(model_to_column).collect();
    Ok(Json(serde_json::json!({ "items": items })))
}

// ── Mappers ─────────────────────────────────────────────────

fn model_to_metric(m: entities::metrics::Model) -> Metric {
    Metric {
        id: m.id,
        insight_tenant_id: m.insight_tenant_id,
        name: m.name,
        description: m.description,
        query_ref: m.query_ref,
        is_enabled: m.is_enabled,
        created_at: m.created_at.naive_utc(),
        updated_at: m.updated_at.naive_utc(),
    }
}

fn model_to_metric_summary(m: entities::metrics::Model) -> MetricSummary {
    MetricSummary {
        id: m.id,
        name: m.name,
        description: m.description,
    }
}

fn model_to_threshold(m: entities::thresholds::Model) -> Threshold {
    Threshold {
        id: m.id,
        insight_tenant_id: m.insight_tenant_id,
        metric_id: m.metric_id,
        field_name: m.field_name,
        operator: m.operator,
        value: m.value,
        level: m.level,
        created_at: m.created_at.naive_utc(),
        updated_at: m.updated_at.naive_utc(),
    }
}

fn model_to_column(m: entities::table_columns::Model) -> TableColumn {
    TableColumn {
        id: m.id,
        insight_tenant_id: m.insight_tenant_id,
        clickhouse_table: m.clickhouse_table,
        field_name: m.field_name,
        field_description: m.field_description,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_query_ref ─────────────────────────────────────

    #[test]
    fn parse_simple_select() {
        let (sel, table, gb) =
            parse_query_ref("SELECT person_id, avg_hours FROM gold.pr_cycle_time").unwrap();
        assert_eq!(sel, "person_id, avg_hours");
        assert_eq!(table, "gold.pr_cycle_time");
        assert!(gb.is_none());
    }

    #[test]
    fn parse_with_group_by() {
        let (sel, table, gb) = parse_query_ref(
            "SELECT person_id, avg(cycle_time_h) AS avg_hours FROM gold.pr_cycle_time GROUP BY person_id",
        )
        .unwrap();
        assert_eq!(sel, "person_id, avg(cycle_time_h) AS avg_hours");
        assert_eq!(table, "gold.pr_cycle_time");
        assert_eq!(gb.as_deref(), Some("person_id"));
    }

    #[test]
    fn parse_case_insensitive() {
        let (sel, table, _) =
            parse_query_ref("select col1, col2 from silver.commits").unwrap();
        assert_eq!(sel, "col1, col2");
        assert_eq!(table, "silver.commits");
    }

    #[test]
    fn parse_with_aggregates_and_group_by() {
        let (sel, table, gb) = parse_query_ref(
            "SELECT org_unit_id, COUNT(DISTINCT person_id) AS headcount, AVG(focus_time_pct) AS focus FROM gold.team_summary GROUP BY org_unit_id",
        )
        .unwrap();
        assert_eq!(
            sel,
            "org_unit_id, COUNT(DISTINCT person_id) AS headcount, AVG(focus_time_pct) AS focus"
        );
        assert_eq!(table, "gold.team_summary");
        assert_eq!(gb.as_deref(), Some("org_unit_id"));
    }

    #[test]
    fn parse_rejects_missing_from() {
        assert!(parse_query_ref("SELECT col1, col2").is_err());
    }

    #[test]
    fn parse_rejects_empty_select() {
        assert!(parse_query_ref("SELECT FROM gold.table").is_err());
    }

    #[test]
    fn parse_rejects_invalid_table_name() {
        assert!(parse_query_ref("SELECT col FROM gold.table; DROP TABLE x").is_err());
    }

    #[test]
    fn parse_rejects_subquery_in_table() {
        assert!(
            parse_query_ref("SELECT col FROM (SELECT * FROM secret.data) AS t").is_err()
        );
    }

    #[test]
    fn parse_rejects_table_with_where() {
        // WHERE in the table position should fail identifier validation
        let result = parse_query_ref("SELECT col FROM gold.t WHERE 1=1");
        assert!(result.is_err());
    }

    // ── is_valid_orderby ────────────────────────────────────

    #[test]
    fn orderby_single_column() {
        assert!(is_valid_orderby("metric_date"));
    }

    #[test]
    fn orderby_with_direction() {
        assert!(is_valid_orderby("metric_date desc"));
        assert!(is_valid_orderby("person_id ASC"));
    }

    #[test]
    fn orderby_multiple_columns() {
        assert!(is_valid_orderby("metric_date desc, person_id asc"));
    }

    #[test]
    fn orderby_dotted_column() {
        assert!(is_valid_orderby("t.metric_date desc"));
    }

    #[test]
    fn orderby_rejects_sql_injection() {
        assert!(!is_valid_orderby("1; DROP TABLE metrics --"));
        assert!(!is_valid_orderby("metric_date; DELETE FROM metrics"));
        assert!(!is_valid_orderby("(SELECT 1)"));
    }

    #[test]
    fn orderby_rejects_empty() {
        assert!(!is_valid_orderby(""));
    }

    #[test]
    fn orderby_rejects_invalid_direction() {
        assert!(!is_valid_orderby("metric_date DROP"));
    }

    // ── is_valid_ident ──────────────────────────────────────

    #[test]
    fn ident_valid() {
        assert!(is_valid_ident("metric_date"));
        assert!(is_valid_ident("gold.pr_cycle_time"));
        assert!(is_valid_ident("col1"));
    }

    #[test]
    fn ident_rejects_special_chars() {
        assert!(!is_valid_ident("col; DROP"));
        assert!(!is_valid_ident("col--"));
        assert!(!is_valid_ident(""));
        assert!(!is_valid_ident(".leading_dot"));
        assert!(!is_valid_ident("trailing_dot."));
    }
}
