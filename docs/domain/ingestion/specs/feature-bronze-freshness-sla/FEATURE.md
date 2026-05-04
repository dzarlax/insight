---
status: proposed
date: 2026-05-04
---

# Feature: Bronze Freshness SLA

Daily check that every Airbyte-managed `bronze_*` source has received fresh
rows within the last 30 hours. The mechanism is a single dbt project policy
plus an Argo `CronWorkflow` — no per-connector code changes are required for
new sources to be covered.

## 1. Feature Context

### 1.1 Overview

Today the ingestion pipeline has no signal for "data is missing". Connectors
that silently stop emitting rows (expired tokens, upstream API outage, Argo
CronWorkflow failures, sync ran but produced 0 rows) only become visible when
a downstream metric goes flat days later.

This feature wires `dbt source freshness` against the `_airbyte_extracted_at`
column that every Airbyte ClickHouse destination writes per row. A source
is `pass` when its MAX(`_airbyte_extracted_at`) is within 30 hours, `warn`
between 30–48 h, `error` past 48 h, and `runtime error` if the freshness
query itself fails.

### 1.2 Purpose

Catch ingestion-layer breaches before they propagate to silver/gold metrics
and to the dashboard. Keep the cost of adding new connectors low — a new
connector inherits the SLA the moment it declares its bronze source.

### 1.3 Actors

| Actor | Role |
|-------|------|
| Ingestion on-call | Reads Argo workflow status; triages `error` runs |
| Connector owner | Receives a follow-up issue from on-call when their connector breaches |
| dbt-source-freshness CronWorkflow | Runs daily at 13:00 UTC, parses `target/sources.json`, fans the result out to a notification webhook |
| Notification consumer (TBD) | Receives the webhook payload — Zulip / email / generic relay |

### 1.4 References

- Operational runbook: [`src/ingestion/MONITORING.md`](../../../../src/ingestion/MONITORING.md) — verification steps, on-call matrix, parser exit codes, payload shape
- Workflow template: [`charts/insight/templates/ingestion/dbt-source-freshness.yaml`](../../../../charts/insight/templates/ingestion/dbt-source-freshness.yaml)
- Threshold config: [`src/ingestion/dbt/dbt_project.yml`](../../../../src/ingestion/dbt/dbt_project.yml) — project-level `+freshness`
- Per-source declarations: every connector's `dbt/schema.yml` carries `loaded_at_field` at source level (dbt does not propagate this property from project config). Streaming connectors anchor on `_airbyte_extracted_at`; report-style connectors (M365 Graph reports, Slack admin.analytics) anchor on the report's own business-day column wrapped in `parseDateTimeBestEffortOrNull(...)`

## 2. Design

### 2.1 Threshold inheritance

```yaml
# src/ingestion/dbt/dbt_project.yml
sources:
  ingestion:
    +freshness:
      warn_after:  { count: 30, period: hour }
      error_after: { count: 48, period: hour }
```

`+freshness` is a config and dbt **does** propagate it to every source under
the project. `loaded_at_field` is a *property*, not a config, so it must be
declared at source level in every connector's `schema.yml`. The
dbt-clickhouse adapter does not support metadata-based freshness — a missing
`loaded_at_field` produces a `runtime error` rather than a sensible default.

#### Streaming vs report-style choice

The choice of `loaded_at_field` is **not** uniform across connectors. Two
patterns exist:

- **Streaming / full-refresh sources** (Jira, Bitbucket, Cursor daily_usage,
  Confluence, Zoom, BambooHR, etc.) — Airbyte's cursor follows business
  time. Rows land in bronze approximately when they happen, so
  `_airbyte_extracted_at` tracks reality. Use `_airbyte_extracted_at`.
- **Report-style sources** (M365 Graph reports, Slack
  admin.analytics.getFile) — Airbyte re-fetches a fixed daily window every
  run. If the upstream stops publishing new days,
  `_airbyte_extracted_at` keeps advancing because the connector still
  writes "fresh" rows for older business days. Anchor on the report's
  business-day column instead, e.g.
  `parseDateTimeBestEffortOrNull(reportRefreshDate)` for Microsoft Graph,
  `parseDateTimeBestEffortOrNull(date)` for Slack analytics.

The local CH dump on 2026-05-04 has the canonical evidence: M365 returned
no new days for 4 days but its sync ran nightly, so `_airbyte_extracted_at`
was 9h old (PASS) while the latest `reportRefreshDate` was 96h old (ERROR).
Slack analytics had the same shape with a 3-day gap. Without the
report-style anchor, both ran silent.

### 2.2 Per-table opt-out

Slow-moving lookup / catalog streams legitimately produce zero new rows on a
quiet day (e.g. `jira_statuses`, `bamboohr.meta_fields`,
`claude_admin_workspaces`). Connector authors mark these with
`freshness: null` next to the table in `schema.yml`.

### 2.3 Workflow

`dbt-source-freshness-check` is a `CronWorkflow` (default schedule
`0 13 * * *` — 13:00 UTC, sits past every connector's sync window of 02:00–
11:00 UTC plus a 2 h grace). Single bash script:

1. `rm -f target/sources.json` — drop any stale report so a mid-run dbt
   crash maps to exit code 2 rather than misclassifying using leftover
   data.
2. `dbt source freshness --select source:*` — non-zero exit is swallowed
   because dbt does not distinguish `warn` from `error` in its exit code.
3. Python parser reads `target/sources.json` and classifies:
   - `pass` only → exit 0, log "all within SLA"
   - `warn` only → exit 0, log breach list
   - any `error` / `runtime error` → exit 1 (Argo Failed), log + POST
     webhook
   - report missing → exit 2

### 2.4 Notification payload

The webhook contract is intentionally provider-agnostic:

```json
{
  "topic": "ingestion-freshness",
  "cluster": "<deployment-id-or-empty>",
  "summary": "[<cluster>] N bronze source(s) breaching freshness SLA",
  "breaches": [
    {
      "source": "source.ingestion.bronze_jira.jira_issue",
      "status": "error",
      "max_loaded_at": "2026-04-28T03:14:21Z",
      "age_hours": 51.2
    }
  ]
}
```

The delivery channel itself is not yet wired — see "Open work" in
`MONITORING.md`. Until then breaches surface only via Argo's failed-runs
list (`failedJobsHistoryLimit: 5`).

## 3. Definitions of Done

### 3.1 Threshold inheritance

- Every `bronze_*` source declared anywhere under
  `src/ingestion/connectors/` is included in `dbt source freshness --select
  source:*` without per-connector wiring.
- New connectors gain coverage by adding `loaded_at_field:
  _airbyte_extracted_at` at the source level — no dbt_project.yml edits.

### 3.2 Per-table opt-out

- `freshness: null` on a table excludes it from the breach count without
  removing the source itself from the report (it appears as `pass`).

### 3.3 Workflow

- Daily run completes within `activeDeadlineSeconds: 1200` for the current
  connector set (~9 sources, ~33 tables; observed end-to-end < 1 s on a
  warm CH).
- A stale dump (no rows newer than 48 h) produces exit code 1 and a log
  entry naming the source, max `_airbyte_extracted_at`, and lag in hours.
- A clean run produces exit code 0 and a one-line "all sources within SLA"
  log.

### 3.4 Notification

- When `notificationWebhookUrl` is empty, the workflow succeeds (or fails
  on `error`) without attempting a fan-out.
- When set, the JSON payload above is POSTed; webhook failures are logged
  but do not change the workflow's primary exit code (the breach signal is
  more important than the delivery success).

### 3.5 Local verification

- `dbt source freshness --select 'source:bronze_*'` runs from the
  `insight-toolbox` image against `clickhouse-local` (HTTP `:8123`,
  `host.docker.internal`) using the workspace dbt profile, with no
  per-source environment-specific overrides.

## 4. Acceptance Criteria

- [ ] Every bronze source under `src/ingestion/connectors/*/dbt/schema.yml`
      declares `loaded_at_field: _airbyte_extracted_at` at source level.
- [ ] `dbt source freshness --select 'source:bronze_*'` from a clean
      checkout produces a non-empty `target/sources.json` with one result
      per declared (source, table) pair, no `runtime error` due to missing
      `loaded_at_field`.
- [ ] CronWorkflow renders cleanly when `templates.enabled=true` and
      `toolboxImage` is pinned (no `:latest` defaults).
- [ ] Stale-data fixture (truncate or backdate `_airbyte_extracted_at` on
      a single source) produces exit code 1 and the source appears in the
      log + payload.
- [ ] On-call matrix and webhook payload contract documented in
      [`MONITORING.md`](../../../../src/ingestion/MONITORING.md).

## 5. Out of Scope

- **Volume baselines** — "API returned 50 rows instead of 5000" is a
  separate iteration tracked in `MONITORING.md`.
- **Source-vs-bronze attribution** — distinguishing "Airbyte sync failed"
  from "Airbyte sync ran but pulled 0 rows" requires a sidecar against the
  Airbyte Jobs API; tracked in `MONITORING.md`.
- **Delivery channel selection** — Zulip vs email vs generic relay; the
  workflow's payload is provider-agnostic so the choice can be deferred.
