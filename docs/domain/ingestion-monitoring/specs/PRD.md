---
status: proposed
date: 2026-05-04
---

# PRD — Ingestion Monitoring

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Scope](#3-scope)
  - [3.1 In Scope](#31-in-scope)
  - [3.2 Out of Scope (deferred to future iterations)](#32-out-of-scope-deferred-to-future-iterations)
- [4. Functional Requirements](#4-functional-requirements)
  - [4.1 Daily freshness check](#41-daily-freshness-check)
  - [4.2 Threshold tiers](#42-threshold-tiers)
  - [4.3 Opt-out hygiene](#43-opt-out-hygiene)
  - [4.4 Trap detection](#44-trap-detection)
  - [4.5 Multi-driver notification](#45-multi-driver-notification)
  - [4.6 Identity labels](#46-identity-labels)
- [5. Non-Functional Requirements](#5-non-functional-requirements)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Dependencies](#7-dependencies)
- [8. Out of Scope / Future Work](#8-out-of-scope--future-work)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Catch ingestion-layer breaches — bronze tables that have stopped receiving
fresh rows, or whose freshness anchor is masking a re-emit pattern — before
the silence propagates to silver/gold metrics and to the dashboard. The
domain owns observability *of what we collect*; it deliberately does not
own deployment health, volume baselines, or vendor-job attribution (see
[§8](#8-out-of-scope--future-work)).

### 1.2 Background / Problem Statement

The ingestion pipeline previously had no signal for "data is missing".
Connectors that silently stopped emitting rows (expired tokens, upstream
API outages, Argo CronWorkflow failures, syncs that ran but produced 0
rows) only became visible when a downstream metric went flat days later.

Two classes of silent failure motivated the domain:

1. **Sync stops, anchor goes stale.** The straightforward case — Airbyte
   connector dies, no new rows land, `MAX(_airbyte_extracted_at)` ages
   out of the warn window.
2. **Sync runs, anchor stays green, upstream has stopped publishing.**
   The trap case. Some connectors (Microsoft Graph reports, Slack
   admin.analytics, Cursor daily_usage, Confluence page versions)
   re-fetch a fixed window every run. Even when the upstream advances
   no new business days, Airbyte keeps writing rows for older days, so
   `_airbyte_extracted_at` looks fresh forever. The local CH dump on
   2026-05-04 had M365 9 hours fresh on `_airbyte_extracted_at` but
   `reportRefreshDate` 96 hours behind reality (see
   [feature-bronze-freshness-sla/FEATURE.md §2.1](feature-bronze-freshness-sla/FEATURE.md)).

### 1.3 Goals (Business Outcomes)

- Every Airbyte-managed `bronze_*` source has a verifiable
  PASS/WARN/ERROR freshness verdict produced once per day.
- Adding a new connector inherits monitoring without per-pipeline
  plumbing — a `loaded_at_field` line in the connector's `schema.yml` is
  enough.
- Operators receive breaches via a notification driver of their choice
  (webhook, Zulip, Slack, Teams, email) — without exposing the channel's
  credentials in rendered manifests or in the Argo UI.
- Connector authors who pick the wrong anchor are caught either at PR
  time (CI lint) or at runtime (trap detector), not by silent green
  dashboards.

### 1.4 Glossary

| Term | Meaning |
|---|---|
| Bronze | The first ClickHouse-side schema written by Airbyte: `bronze_<connector>.<stream>`. |
| Freshness anchor | The SQL expression in `loaded_at_field` against which `dbt source freshness` measures lag. |
| Streaming connector | Connector whose Airbyte cursor follows business time; rows land in bronze approximately when they happen. Anchor = `_airbyte_extracted_at`. |
| Report-style connector | Connector that re-fetches a fixed window every run (Microsoft Graph reports, Slack admin.analytics). Anchor = a business-date column. |
| Event-style connector | Streaming connector with legitimate quiet days (Confluence edits, Zoom meetings). Uses a wider tier. |
| SLA tier | A pair `(warn_after, error_after)` shared by connectors with similar cadence: `default`, `event`, `report`, `report_extended`. |
| Trap | A bronze table whose `_airbyte_extracted_at` looks fresh while the upstream has stopped publishing — the freshness check passes for the wrong reason. |
| Driver | A notification dispatch backend selected via `ingestion.freshness.notification.driver`: `webhook` / `zulip` / `slack` / `teams` / `email`. |

## 2. Actors

### 2.1 Human Actors

| Actor | Role | Reads | Acts on |
|---|---|---|---|
| Ingestion on-call | Triages freshness breaches | Argo workflow status / notification payload | `error` and `runtime error` runs |
| Connector owner | Owns a specific `bronze_*` source | Issue handed off by on-call | Fixes the connector or revisits the SLA tier |
| Tenant on-call (post-MVP) | Per-deployment triage when one bot fans out to multiple installations | Notification payload's `cluster` / `tenant` labels | Same triage as ingestion on-call, scoped to one deployment |

Rotation / `CODEOWNERS` for `src/ingestion/connectors/` is **not yet
assigned** — see
[`src/ingestion/MONITORING.md` §"Rotation / ownership — not assigned"](../../../../src/ingestion/MONITORING.md).

### 2.2 System Actors

| Actor | Role |
|---|---|
| `dbt-source-freshness` `WorkflowTemplate` (Argo) | Stateless template — runs `dbt source freshness`, parses `target/sources.json`, dispatches to the active driver. Defined in [`charts/insight/templates/ingestion/dbt-source-freshness.yaml`](../../../../charts/insight/templates/ingestion/dbt-source-freshness.yaml). |
| `dbt-source-freshness-check` `CronWorkflow` (Argo) | Schedules the template once a day at 13:00 UTC; carries the per-deployment Helm config into the template's parameters. Same file. |
| Inline freshness parser (Python) | Reads `target/sources.json`, classifies breaches, calls the driver dispatcher, sets the workflow exit code. Inline in the workflow template, lines 317–562. |
| Trap detector ([`src/ingestion/scripts/freshness-trap-detect.py`](../../../../src/ingestion/scripts/freshness-trap-detect.py)) | Advisory script — spots full-reemit and incremental-topup patterns that the dbt freshness check cannot see. Runs after the parser. |
| CI lint ([`src/ingestion/scripts/lint-bronze-freshness.py`](../../../../src/ingestion/scripts/lint-bronze-freshness.py)) | Fails any PR that introduces a `bronze_*` source without a reachable `loaded_at_field`, or a `freshness: null` opt-out without `meta.freshness_optout_reason`. |

## 3. Scope

### 3.1 In Scope

- **Bronze freshness SLA** — the `dbt source freshness` check against
  `_airbyte_extracted_at` (streaming) or a business-date column
  (report/event), with four threshold tiers (`default` 30/48 h, `event`
  72/96 h, `report` 48/96 h, `report_extended` 72/120 h). Source:
  [`feature-bronze-freshness-sla/FEATURE.md`](feature-bronze-freshness-sla/FEATURE.md).
- **CI lint of connector schemas** — every `bronze_*` source must
  declare a reachable `loaded_at_field` (source-level, per-table, or
  explicit `freshness: null` with `meta.freshness_optout_reason`).
  Source: [`lint-bronze-freshness.py`](../../../../src/ingestion/scripts/lint-bronze-freshness.py).
- **Runtime trap detector** — heuristic full-reemit detection (no
  config) plus opt-in `meta.bronze_business_date_col` divergence check;
  advisory only, never affects workflow exit code. Source:
  [`freshness-trap-detect.py`](../../../../src/ingestion/scripts/freshness-trap-detect.py).
- **Multi-driver notification fan-out** — five drivers
  (webhook / zulip / slack / teams / email) selectable via Helm; URL /
  SMTP password sourced from a Kubernetes Secret so the credential
  never appears in rendered manifests or Argo UI. Source: workflow
  template lines 161–194 and 412–540.
- **Identity labels in payload** — `cluster` and `tenant` carried into
  every payload's summary prefix and JSON body so a shared bot can
  route between installations.

### 3.2 Out of Scope (deferred to future iterations)

- **Deployment health** — "is a connector that should be deployed
  actually deployed?" is a separate concern (Helm probes / sync
  workflow status). Tracked as
  [issue #272](https://github.com/cyberfabric/insight/issues/272). The
  freshness workflow narrows its selector to bronze databases that
  exist (workflow template lines 254–297) precisely so it does *not*
  alert on connectors a tenant hasn't deployed.
- **Volume baseline anomaly detection** — "API returned 50 rows
  instead of 5000". Listed under "Open work" in
  [`MONITORING.md`](../../../../src/ingestion/MONITORING.md).
- **Source-vs-bronze attribution** — distinguishing "Airbyte sync
  failed" from "Airbyte sync ran but pulled 0 rows" requires polling
  Airbyte's Jobs API; listed under "Open work" in
  [`MONITORING.md`](../../../../src/ingestion/MONITORING.md).
- **Notification rotation / `CODEOWNERS`** — see
  [`MONITORING.md` §"Rotation / ownership — not assigned"](../../../../src/ingestion/MONITORING.md).

## 4. Functional Requirements

### 4.1 Daily freshness check

- A single Argo `CronWorkflow` (`dbt-source-freshness-check`) runs once
  per day on the schedule supplied by `ingestion.freshness.schedule`
  (default `0 13 * * *`, charts/insight/values.yaml:151).
- The workflow narrows the dbt selector from `source:*` to the
  `bronze_*` databases that actually exist in `system.databases`
  (workflow template lines 262–297) — connectors a tenant did not
  deploy never produce a phantom ERROR.
- A stale `target/sources.json` is removed before each run (workflow
  template line 252) so a mid-run dbt crash maps to exit code 2 rather
  than misclassifying using leftover data.
- `dbt source freshness` is invoked with `--select <effective>` and its
  exit code is swallowed (line 305–307). The Python parser re-derives
  the workflow outcome from `target/sources.json`.

### 4.2 Threshold tiers

- All thresholds are sourced once from `ingestion.freshness.thresholds.*`
  (charts/insight/values.yaml:159–183), passed as `WorkflowTemplate`
  parameters, exported as `FRESHNESS_*_H` env vars (workflow template
  lines 197–212), and read by dbt via
  `env_var('FRESHNESS_WARN_DEFAULT_H', '30')` in
  [`src/ingestion/dbt/dbt_project.yml`](../../../../src/ingestion/dbt/dbt_project.yml)
  and per-connector `schema.yml`.
- Four tiers, each with documented rationale:
  - `default` (30/48 h) — streaming connectors with daily cron cadence.
  - `event` (72/96 h) — natural-quiet-day connectors (Confluence
    edits, Zoom meetings).
  - `report` (48/96 h) — vendor analytics with documented 24–48 h
    publish lag (Microsoft Graph reports baseline).
  - `report_extended` (72/120 h) — vendor analytics with ~3-day
    baseline lag (Slack admin.analytics typical).
- Tier assignment is per-connector and lives in the connector's
  `schema.yml`. Re-tiering is an engineering change; Helm tunes the
  *values* of each tier, not which source falls in which tier.

### 4.3 Opt-out hygiene

- Slow-moving roster / catalog tables (e.g. `cursor_members`,
  `wiki_spaces`) opt out via `freshness: null` per-table in
  `schema.yml`.
- The CI lint
  ([`lint-bronze-freshness.py`](../../../../src/ingestion/scripts/lint-bronze-freshness.py))
  rejects any opt-out lacking
  `meta.freshness_optout_reason: "<rationale>"` so the audit surface
  stays grep-able.
- The lint also rejects any `bronze_*` source with no reachable
  `loaded_at_field` (source-level, per-table, or explicit opt-out).

### 4.4 Trap detection

- After the freshness parser exits, the workflow runs
  [`freshness-trap-detect.py`](../../../../src/ingestion/scripts/freshness-trap-detect.py)
  (workflow template lines 564–579).
- Two detection modes:
  - **Full re-emit (heuristic, no config)** — fires when ≥ 95 % of
    rows have `_airbyte_extracted_at` within the last 30 h *and*
    `_airbyte_extracted_at` covers ≤ 2 distinct calendar days *and*
    the table has ≥ 100 rows (script constants
    `SUSPECT_PCT_RECENT`, `SUSPECT_MAX_DISTINCT_DAYS`,
    `RECENT_WINDOW_HOURS`, `MIN_ROWS`).
  - **Business-date divergence (opt-in)** — sources that declare
    `meta.bronze_business_date_col: <SQL expr>` get
    `MAX(<expr>)` compared with `MAX(_airbyte_extracted_at)`; a
    ≥ 24 h gap is flagged.
- A source can opt out of mode 1 via
  `meta.bronze_freshness_trap_check: skip` at source or table level.
- Findings are advisory: `trap_exit=1` only logs a notice. The
  workflow's page/no-page decision remains owned by the freshness
  parser (workflow template lines 575–579).

### 4.5 Multi-driver notification

- `ingestion.freshness.notification.driver` selects one of `""`,
  `webhook`, `zulip`, `slack`, `teams`, `email`
  (charts/insight/values.yaml:220, schema enum at
  charts/insight/values.schema.json:37).
- Each URL-driven driver reads its credential from a `secretKeyRef`
  branched in the workflow template (lines 161–194) — the URL is
  bound to `NOTIFICATION_URL` only when the driver is configured;
  email's SMTP password binds to
  `NOTIFICATION_EMAIL_SMTP_PASSWORD`. Plain settings (Zulip stream,
  Slack channel, SMTP host/port) flow through normal parameters
  because they are not credentials.
- Driver dispatch happens in the inline parser via the `dispatchers`
  dict (workflow template lines 533–539).
- Notification failures are caught and logged (lines 550–554) — they
  never change the workflow's primary exit code (the breach signal
  is more important than the delivery success).

### 4.6 Identity labels

- `ingestion.freshness.cluster` and `.tenant` (values.yaml:193–194)
  flow into the parser as `CLUSTER` / `TENANT` env vars.
- The summary line carries every set label as a
  `[cluster=…, tenant=…] ` prefix (workflow template lines 393–397);
  empty labels drop out so single-deployment installs see plain
  `N bronze source(s) breaching ...`.
- The webhook payload includes both `cluster` and `tenant` as raw
  fields so receivers can route by either dimension.

## 5. Non-Functional Requirements

| ID | Requirement |
|---|---|
| NFR-1 | Workflow is **idempotent** — re-running it on the same data produces the same exit code and the same payload (no stateful side-effects, `target/sources.json` is purged before each run). |
| NFR-2 | **Daily cadence** by default; cron schedule is Helm-tunable but the parser is stateless and can run ad-hoc. |
| NFR-3 | **No credentials in rendered manifests** — webhook URL / SMTP password are pulled from k8s Secrets via `secretKeyRef`; the rendered `WorkflowTemplate`/`CronWorkflow` YAML and the Argo UI never see the raw value. |
| NFR-4 | **Deterministic exit codes** — `0` no breach or warn-only; `1` at least one `error`/`runtime error`; `2` `target/sources.json` missing (dbt crashed). |
| NFR-5 | **CI-gated schema** — `lint-bronze-freshness.py` runs on every PR touching `src/ingestion/connectors/*/dbt/schema.yml`. |
| NFR-6 | **Advisory-fail mode for traps** — trap detector findings never override the freshness parser's verdict (page-worthy stays page-worthy; warn-only stays warn-only). |
| NFR-7 | **Activation deadline** — workflow has `activeDeadlineSeconds: 1200` (workflow template line 110); current connector set finishes in ~2 min on a warm CH. |
| NFR-8 | **Empty-table sentinel** — a source with zero rows yields `MAX(...) = NULL → 1970-01-01T…`; the parser flags it explicitly as `(table is empty — no rows ever ingested)` rather than reporting ~500 000 h of lag (workflow template lines 348–360). |

## 6. Acceptance Criteria

- [ ] Every `bronze_*` source under `src/ingestion/connectors/*/*/dbt/schema.yml` has a reachable `loaded_at_field` (source / table / opt-out) — verified by `python3 src/ingestion/scripts/lint-bronze-freshness.py` exiting 0.
- [ ] Every `freshness: null` opt-out carries `meta.freshness_optout_reason` — same lint enforces this.
- [ ] `dbt source freshness --select 'source:bronze_*'` from a clean checkout produces `target/sources.json` with one result per declared (source, table) pair, no `runtime error` due to missing `loaded_at_field`.
- [ ] Each tier produces the expected verdict on its representative connectors:
  - `default` — Bitbucket, Jira, BambooHR, Cursor (non-daily), GitHub, OpenAI, Claude (PASS within 30 h of last sync).
  - `event` — Confluence `wiki_page_versions`, Zoom `meetings`/`participants` (PASS within 72 h, weekend-tolerant).
  - `report` — M365 `*_activity` (PASS within 48 h of `reportRefreshDate`).
  - `report_extended` — Slack `users_details` (PASS within 72 h of `date`).
- [ ] A stale-data fixture (truncate or backdate `_airbyte_extracted_at` on one source) produces parser exit code 1, a log line naming the source / `max_loaded_at` / `age_hours`, and a notification payload (when a driver is configured).
- [ ] The notification payload always carries both `cluster` and `tenant` keys (empty string when unset), matching the canonical webhook contract documented in [`MONITORING.md`](../../../../src/ingestion/MONITORING.md).
- [ ] The configured driver's credential **does not appear** in `kubectl get workflowtemplate dbt-source-freshness -o yaml` or in `argo get` output — only the `secretKeyRef` does (NFR-3).
- [ ] An empty `bronze_*` table is reported as `(table is empty)` instead of `~500000h` lag (NFR-8 / workflow template lines 348–360).
- [ ] Synthetic Confluence-style fixture (full table re-emit within 24 h, 1 distinct extract day, ≥ 100 rows) is flagged by the trap detector with `kind=full-reemit` (script lines 188–200).
- [ ] Synthetic incremental-topup fixture (`MAX(_airbyte_extracted_at)` fresh, `MAX(<bronze_business_date_col>)` ≥ 24 h behind) is flagged with `kind=incremental-topup` (script lines 202–227).
- [ ] `values.schema.json` rejects an unknown driver name (only `""`, `webhook`, `zulip`, `slack`, `teams`, `email` are accepted — schema line 37).

## 7. Dependencies

- **Argo Workflows** controller in the `argo` namespace — provides
  `WorkflowTemplate` / `CronWorkflow` reconciliation and the
  `failedJobsHistoryLimit` retention used as the no-fan-out fallback.
- **dbt-clickhouse** adapter inside the `insight-toolbox` image —
  provides `dbt source freshness` and `target/sources.json`. The
  adapter does not support metadata-based freshness, so a missing
  `loaded_at_field` produces `runtime error` rather than silent skip.
- **ClickHouse** — readable `system.databases` (for selector
  narrowing) and HTTP interface on port 8123 (used by the trap
  detector).
- **Kubernetes Secret access** — `secretKeyRef` for both
  `clickhouse.passwordSecret` and the active driver's credential
  Secret.
- **Helm umbrella chart** — `ingestion.freshness.*` block in
  `charts/insight/values.yaml`, validated by
  `charts/insight/values.schema.json`.

## 8. Out of Scope / Future Work

| Item | Where it's tracked |
|---|---|
| Deployment health (Helm probes / sync workflow status) | [issue #272](https://github.com/cyberfabric/insight/issues/272) |
| Volume baseline anomaly detection (today vs 14-day median) | [`MONITORING.md` §"Volume baseline (next iteration)"](../../../../src/ingestion/MONITORING.md) |
| Source vs bronze attribution (Airbyte Jobs API sidecar) | [`MONITORING.md` §"Source vs bronze attribution"](../../../../src/ingestion/MONITORING.md) |
| Ingestion on-call rotation + per-connector `CODEOWNERS` | [`MONITORING.md` §"Rotation / ownership — not assigned"](../../../../src/ingestion/MONITORING.md) |
| Argo `onExit` retry / templating notifications | [`MONITORING.md` §"Delivery channel — to be decided"](../../../../src/ingestion/MONITORING.md) |
