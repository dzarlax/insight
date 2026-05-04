---
status: proposed
date: 2026-05-04
---

# Technical Design — Ingestion Monitoring

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 Workflow Inputs](#33-workflow-inputs)
  - [3.4 Driver Contracts](#34-driver-contracts)
  - [3.5 SLA Tier Mapping](#35-sla-tier-mapping)
  - [3.6 Notification Payload](#36-notification-payload)
  - [3.7 Failure Semantics](#37-failure-semantics)
  - [3.8 Helm Surface](#38-helm-surface)
- [4. Decisions / Why we did X](#4-decisions--why-we-did-x)
  - [4.1 Why two report tiers (`report`, `report_extended`) instead of one](#41-why-two-report-tiers-report-reportextended-instead-of-one)
  - [4.2 Why `_airbyte_extracted_at` for streaming and a business date for re-emit](#42-why-airbyteextractedat-for-streaming-and-a-business-date-for-re-emit)
  - [4.3 Why per-driver subblocks instead of a flat `notification.url`](#43-why-per-driver-subblocks-instead-of-a-flat-notificationurl)
  - [4.4 Why the Zulip dispatcher silently rewrites the endpoint](#44-why-the-zulip-dispatcher-silently-rewrites-the-endpoint)
  - [4.5 Why narrow the selector at runtime](#45-why-narrow-the-selector-at-runtime)
  - [4.6 Why the trap detector is advisory](#46-why-the-trap-detector-is-advisory)
  - [4.7 Why `meta.freshness_optout_reason` is mandatory](#47-why-metafreshnessoptoutreason-is-mandatory)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

Ingestion Monitoring is a single daily-cadence Argo `CronWorkflow`
(`dbt-source-freshness-check`) that delegates to one stateless
`WorkflowTemplate` (`dbt-source-freshness`). The template runs `dbt
source freshness` against ClickHouse, parses `target/sources.json` with
an inline Python step, and dispatches breaches to one of five
notification drivers (webhook / zulip / slack / teams / email). Two
scripts sit at the edges: a CI lint that prevents bad anchors from
landing, and a runtime trap detector that flags re-emit patterns the
freshness check itself cannot see.

The design's defining property is **everything is config-driven from a
single Helm block** (`ingestion.freshness.*` in
[`charts/insight/values.yaml`](../../../../charts/insight/values.yaml)).
Adding a connector is a `schema.yml` edit; switching notification
channels is a `values.yaml` edit; tuning thresholds is a `values.yaml`
edit. No Rust services, no extra Deployments, no per-connector pipeline
plumbing.

The second defining property is **credential isolation by
construction**. The driver's URL or SMTP password is bound to the pod's
env from a `secretKeyRef` resolved server-side by Argo; the rendered
manifest, the Argo UI, and the workflow-controller logs only ever see
the Secret reference, never the raw value
([`charts/insight/templates/ingestion/dbt-source-freshness.yaml:161-194`](../../../../charts/insight/templates/ingestion/dbt-source-freshness.yaml)).

### 1.2 Architecture Drivers

**Functional drivers**

- Silent staleness was the original trigger — connectors stopping
  silently is the failure mode dashboards are blind to.
- Vendor-documented multi-day publish lag (Microsoft Graph reports,
  Slack admin.analytics, Zoom 30-day re-fetch) means a single
  threshold cannot cover all sources; we need a tier system.
- The freshness check on `_airbyte_extracted_at` is fooled by
  re-emit / incremental-topup patterns; we need a second, independent
  detector.

**Quality drivers**

- **Multi-tenant routing.** One bot may fan out to multiple
  installations, so the payload must carry `cluster` and `tenant`
  labels.
- **Credential isolation.** Webhook URLs are the credential — once
  leaked, anyone can post into the customer's Zulip / Slack. Putting
  them in `values.yaml` is unsafe; they must come from a Secret.
- **Cheap to add a connector.** New connectors must inherit the SLA
  by declaring a single line in their `schema.yml`.
- **Cheap to add a driver.** A new driver should be one match arm in
  the parser + one Helm subblock + one `secretKeyRef` branch — no
  changes to the freshness check itself.

### 1.3 Architecture Layers

```
ingestion.freshness.* (charts/insight/values.yaml)
        │
        ▼
CronWorkflow.spec.workflowSpec parameters
(charts/insight/templates/ingestion/dbt-source-freshness.yaml:583-662)
        │
        ▼
WorkflowTemplate inputs.parameters → env vars + secretKeyRef
(.yaml:34-212)
        │
        ▼
dbt source freshness  ─►  target/sources.json
(env_var(...) reads FRESHNESS_*_H)
        │
        ▼
Inline Python parser (.yaml:317-562)
        │      │
        │      └─► driver dispatcher (webhook / zulip / slack / teams / email)
        │              │
        │              └─► external sink (Zulip / Slack / Teams / SMTP / generic)
        ▼
freshness-trap-detect.py (advisory)
(src/ingestion/scripts/freshness-trap-detect.py)
```

## 2. Principles & Constraints

### 2.1 Design Principles

| Principle | What it means in this codebase |
|---|---|
| **Advisory failure mode for traps** | The trap detector logs but never overrides the freshness parser's exit code (workflow template lines 575–579). Page-worthy decisions belong to one place. |
| **Log loud on failure** | Notification HTTP errors surface the upstream's body (workflow template lines 421–432) — the `Python-urllib` default exposes only the status line, which hides Zulip / Slack / Teams JSON error payloads. |
| **Scope freshness to what's collected** | Selector is narrowed from `source:*` to `bronze_*` databases that exist in `system.databases` (lines 262–297); a tenant that didn't deploy OpenAI doesn't see phantom OpenAI ERRORs. |
| **Separate from deployment health** | "Is connector X actually deployed?" is issue [#272](https://github.com/cyberfabric/insight/issues/272), not this domain. |
| **Vendor-documented thresholds, not gut feeling** | Each tier's warn / error pair sits at the upper edge of the vendor's documented normal cadence (M365 24–48 h → warn 48 h; Slack ~3 d → warn 72 h). Documented in `values.yaml` lines 156–183. |

### 2.2 Constraints

- **dbt-clickhouse needs explicit `loaded_at_field`.** The adapter
  does not support metadata-based freshness; a missing field → `runtime
  error`. `+loaded_at_field` at project level is silently ignored
  (`loaded_at_field` is a *property*, not a config). Hence the
  per-source declaration and the lint that enforces it.
- **`+freshness` *does* propagate.** The project-level `+freshness`
  block in `dbt_project.yml:71-79` is inherited by every source;
  per-source `freshness:` blocks override it.
- **Zulip's `/external/json` is not a markdown sender.** It dumps the
  request body as a JSON code block. The Slack-compatible endpoint
  (`/external/slack`) renders Slack-style markdown. The parser
  silently rewrites the path so an operator who pasted the JSON URL
  still gets formatted output (workflow template lines 445–478).
- **Cloudflare-managed receivers reject the default UA.** A bare
  `Python-urllib/3.x` looks like a bot. The dispatcher sets an
  explicit `User-Agent: insight-freshness-monitor/1.0` (workflow
  template line 416).
- **Zulip stream names can carry zero-width-space (U+200B).** Copying
  the integration URL from Zulip's UI is safer than retyping the
  stream name (`charts/insight/values.yaml:243-249`).
- **`MAX(<col>)` over zero rows is `NULL`.** dbt-clickhouse serialises
  it as the Unix epoch with an enormous `age_in_s`, which reads as
  ~500 000 h of lag. The parser detects the sentinel
  (`max_loaded_at.startswith("1970-01-01")`) and surfaces "table is
  empty (no rows ingested)" instead (workflow template lines 348–360).

## 3. Technical Architecture

### 3.1 Domain Model

| Concept | Where it's defined |
|---|---|
| **Bronze source** | `sources:` entry in any connector's `dbt/schema.yml`; identified by `name: bronze_*` or `schema: bronze_*` (lint matches both — see `lint-bronze-freshness.py:69-73`). |
| **Freshness anchor** | `loaded_at_field` property at source or table level. Two valid forms: `_airbyte_extracted_at` (streaming) or a business-date expression (`parseDateTimeBestEffortOrNull(...)`, `parseDateTime64BestEffortOrNull(..., 3)`, `fromUnixTimestamp64Milli(...)`). |
| **Opt-out** | Per-table `freshness: null` plus mandatory `meta.freshness_optout_reason: "<rationale>"` (lint at `lint-bronze-freshness.py:88-97`). |
| **SLA tier** | One of `default`, `event`, `report`, `report_extended`; chosen by the connector's source-level `freshness:` block referencing a tier-specific env var. |
| **Breach** | A row in the parser's `breaches` list: `{source, status, max_loaded_at, age_hours, empty}` (workflow template lines 352–360). |
| **Trap suspect** | A finding from the trap detector: `kind ∈ {full-reemit, incremental-topup}` plus row-level evidence (`freshness-trap-detect.py:186-227`). |
| **Driver** | A parser dispatch arm + Helm subblock + workflow-yaml `secretKeyRef` branch. Five today (`webhook`, `zulip`, `slack`, `teams`, `email`); `""` = log-only. |

### 3.2 Component Model

| Component | File | Role |
|---|---|---|
| `dbt-source-freshness` (`WorkflowTemplate`) | `charts/insight/templates/ingestion/dbt-source-freshness.yaml:24-212` | Stateless template — accepts every per-deployment parameter via `inputs.parameters`, binds credentials via `secretKeyRef`. |
| `dbt-source-freshness-check` (`CronWorkflow`) | same file, lines 583–662 | Wires Helm values into the template's parameters. |
| Inline freshness parser | same file, lines 317–562 | Reads `target/sources.json`, classifies, dispatches, sets exit code. |
| dbt project freshness defaults | `src/ingestion/dbt/dbt_project.yml:70-79` | `+freshness` block reading `env_var('FRESHNESS_*_H', '<default>')`. |
| Per-connector schema | `src/ingestion/connectors/*/*/dbt/schema.yml` | Source-level `loaded_at_field` + optional `freshness:` block selecting a tier; per-table `freshness: null` opt-outs with `meta.freshness_optout_reason`. |
| CI lint | `src/ingestion/scripts/lint-bronze-freshness.py` | Two checks: reachable anchor and rationale on every opt-out. |
| Trap detector | `src/ingestion/scripts/freshness-trap-detect.py` | Two-mode advisory check: full-reemit heuristic and `meta.bronze_business_date_col` divergence. |
| Helm surface | `charts/insight/values.yaml:147-292`, schema `charts/insight/values.schema.json:26-99` | Single tunable surface. |

### 3.3 Workflow Inputs

Sourced verbatim from
[`charts/insight/templates/ingestion/dbt-source-freshness.yaml:34-106`](../../../../charts/insight/templates/ingestion/dbt-source-freshness.yaml).

| Parameter | Default | Purpose |
|---|---|---|
| `dbt_select` | `source:*` | Selector for `dbt source freshness`. Workflow narrows it to deployed `bronze_*` databases unless caller supplies a more specific selector. |
| `toolbox_image` | (required) | Image carrying dbt + dbt-clickhouse + the freshness scripts. |
| `clickhouse_host` | (required) | CH HTTP host. |
| `clickhouse_port` | (required) | CH HTTP port. |
| `clickhouse_user` | (required) | CH user. CH password comes from `clickhouse.passwordSecret` via `secretKeyRef`. |
| `cluster` | `""` | Identification label — installation tier. |
| `tenant` | `""` | Identification label — customer / workspace. |
| `notification_driver` | `""` | One of `""`, `webhook`, `zulip`, `slack`, `teams`, `email`. |
| `notification_zulip_stream` | `""` | Zulip stream override (else use what's encoded in the integration URL). |
| `notification_zulip_topic` | `""` | Zulip topic override. |
| `notification_slack_channel` | `""` | Slack channel override (subject to workspace policy). |
| `notification_email_smtp_host` | `""` | SMTP host (required when driver=email). |
| `notification_email_smtp_port` | `"587"` | 587 = STARTTLS, 465 = SMTPS, 25 = plain. |
| `notification_email_smtp_username` | `""` | SMTP AUTH username; password lives in a Secret. |
| `notification_email_smtp_starttls` | `"true"` | Ignored when port=465. |
| `notification_email_from` | `""` | Envelope sender. |
| `notification_email_to` | `""` | Comma-separated recipient list. |
| `notification_email_subject_prefix` | `"[ingestion-freshness]"` | Prefix prepended to the summary. |
| `warn_default_h` / `error_default_h` | `30` / `48` | "default" tier (streaming connectors). |
| `warn_event_h` / `error_event_h` | `72` / `96` | "event" tier (natural quiet days). |
| `warn_report_h` / `error_report_h` | `48` / `96` | "report" tier (24–48 h vendor publish lag). |
| `warn_report_extended_h` / `error_report_extended_h` | `72` / `120` | "report_extended" tier (3-day vendor baseline). |

The credential parameters (`NOTIFICATION_URL` for URL-driven drivers,
`NOTIFICATION_EMAIL_SMTP_PASSWORD` for email) are **not** workflow
parameters; they bind via `valueFrom.secretKeyRef` (lines 161–194). The
binding is rendered conditionally on `$n.driver`, so an unconfigured
cluster (driver="") does not try to resolve a non-existent Secret.

### 3.4 Driver Contracts

The driver dispatcher is the `dispatchers` dict at workflow template
lines 533–539. Common preamble: every dispatch goes through `_post`
(lines 412–432), which sets `User-Agent: insight-freshness-monitor/1.0`
and surfaces the upstream's response body on `HTTPError`.

#### 3.4.1 `webhook`

- Endpoint: whatever is in `NOTIFICATION_URL` (verbatim).
- Body shape (workflow template lines 434–443):

```json
{
  "topic": "ingestion-freshness",
  "cluster": "<cluster-or-empty>",
  "tenant": "<tenant-or-empty>",
  "summary": "[cluster=…, tenant=…] N bronze source(s) breaching freshness SLA",
  "breaches": [
    {
      "source": "source.ingestion.bronze_jira.jira_issue",
      "status": "error",
      "max_loaded_at": "2026-04-28T03:14:21Z",
      "age_hours": 51.2,
      "empty": false
    }
  ]
}
```

- Header: `Content-Type: application/json`.
- Secret binding: `ingestion.freshness.notification.webhook.urlSecret.{name,key}` (default `key: url`), workflow YAML lines 162–167.

#### 3.4.2 `zulip`

- Endpoint: `NOTIFICATION_URL` with the path `/api/v1/external/json` rewritten to `/api/v1/external/slack` (line 457). The two paths are different Zulip integrations — `/external/json` dumps the body as a JSON code block (debugging integration); `/external/slack` is the Slack-compatible incoming webhook that renders Slack mrkdwn. Operators who pasted the JSON URL still get a formatted message.
- Routing: `stream` and `topic` from `NOTIFICATION_ZULIP_STREAM` / `NOTIFICATION_ZULIP_TOPIC` are URL-encoded and appended to the query string (lines 458–462).
- Body: `application/x-www-form-urlencoded` carrying `user_name=ingestion-freshness`, `channel_name=freshness`, `text=<slack-mrkdwn>` — the Slack-compatible webhook expects these legacy-Slack form fields (lines 472–478).
- Markdown: single `*` for bold (Slack mrkdwn), bullet `•` (line 466).
- Secret binding: `ingestion.freshness.notification.zulip.urlSecret.{name,key}` (workflow YAML lines 168–173).
- Quirk: Zulip stream names can include trailing zero-width-space U+200B; `values.yaml:243-249` warns operators to copy from Zulip's UI rather than retype.

#### 3.4.3 `slack`

- Endpoint: `NOTIFICATION_URL` (Slack incoming-webhook URL).
- Body: `application/json` with `{"text": "<markdown>"}`, optionally `channel: <override>` if `NOTIFICATION_SLACK_CHANNEL` is set and workspace policy allows it (lines 480–487).
- Secret binding: `ingestion.freshness.notification.slack.urlSecret.{name,key}` (workflow YAML lines 174–179).

#### 3.4.4 `teams`

- Endpoint: `NOTIFICATION_URL` (Microsoft Teams incoming webhook).
- Body: `application/json`, `MessageCard` schema (lines 489–500). `themeColor` is `FF0000` (red) when any breach is page-worthy, `FFA500` (orange) otherwise.
- No channel override — Teams binds the URL to a single channel at creation.
- Secret binding: `ingestion.freshness.notification.teams.urlSecret.{name,key}` (workflow YAML lines 180–185).

#### 3.4.5 `email`

- Transport: `smtplib` from the standard library (no extra deps).
  `SMTP_SSL` when port=465, otherwise plain `SMTP` with optional
  `STARTTLS` (lines 502–531).
- Required Helm values: `notification.email.smtp.host`,
  `notification.email.from`, `notification.email.to`. The dispatcher
  raises `RuntimeError` if any is missing (lines 513–517).
- Subject: `<prefix> <summary>` when prefix is non-empty.
- Recipients: comma-separated `NOTIFICATION_EMAIL_TO` split on `,`,
  whitespace-stripped.
- Secret binding: `ingestion.freshness.notification.email.smtp.passwordSecret.{name,key}` (workflow YAML lines 187–193, rendered only when `$n.email.smtp.passwordSecret.name` is set).

### 3.5 SLA Tier Mapping

Default tier values (from
[`charts/insight/values.yaml:159-183`](../../../../charts/insight/values.yaml)):

| Tier | warn | error | Env var (warn / error) | Connectors using it |
|---|---|---|---|---|
| `default` | 30 h | 48 h | `FRESHNESS_WARN_DEFAULT_H` / `FRESHNESS_ERROR_DEFAULT_H` | bamboohr, bitbucket-cloud, claude-admin, claude-enterprise, github, jira, openai, cursor (non-daily-usage tables), m365 *(see note below)*, slack `users_details` *(no — see report_extended)*, cursor `cursor_daily_usage` |
| `event` | 72 h | 96 h | `FRESHNESS_WARN_EVENT_H` / `FRESHNESS_ERROR_EVENT_H` | confluence `wiki_page_versions`, zoom `meetings`, zoom `participants` |
| `report` | 48 h | 96 h | `FRESHNESS_WARN_REPORT_H` / `FRESHNESS_ERROR_REPORT_H` | m365 `*_activity` (Microsoft Graph reports — 24–48 h documented publish lag) |
| `report_extended` | 72 h | 120 h | `FRESHNESS_WARN_REPORT_EXTENDED_H` / `FRESHNESS_ERROR_REPORT_EXTENDED_H` | slack `users_details` (admin.analytics — ~3 d typical, up to 5 d during Slack maintenance) |

Sample anchor + tier choices — full table in
[`src/ingestion/MONITORING.md`](../../../../src/ingestion/MONITORING.md)
"What's wired" section.

| Connector / table | `loaded_at_field` | Tier |
|---|---|---|
| `bronze_m365.{teams,email,onedrive,sharepoint}_activity` | `parseDateTimeBestEffortOrNull(reportRefreshDate)` | `report` |
| `bronze_slack.users_details` | `parseDateTimeBestEffortOrNull(date)` | `report_extended` |
| `bronze_confluence.wiki_page_versions` | `parseDateTime64BestEffortOrNull(created_at, 3)` | `event` |
| `bronze_cursor.cursor_daily_usage` | `fromUnixTimestamp64Milli(toInt64OrZero(toString(date)))` | `default` |
| `bronze_cursor.<other tables>` | `_airbyte_extracted_at` | `default` |
| `bronze_zoom.meetings` | `parseDateTimeBestEffortOrNull(start_time)` | `event` |
| `bronze_zoom.users` | (opted out via `freshness: null`) | — |

Re-tiering a connector is an engineering change (it usually rides
together with a `loaded_at_field` revisit) — the tier mapping lives in
connector `schema.yml`, not in Helm values.

### 3.6 Notification Payload

#### 3.6.1 Canonical webhook shape

Same JSON as in §3.4.1. The summary's bracket prefix carries each set
identity label (workflow template lines 393–397):

- Both set → `"[cluster=prod, tenant=acme] 3 bronze source(s) breaching freshness SLA"`
- Only `cluster` → `"[cluster=prod] 3 bronze source(s) ..."`
- Neither → `"3 bronze source(s) breaching freshness SLA"`

The `breaches[i]` object always carries the same keys (workflow
template lines 352–360):

| Key | Type | Notes |
|---|---|---|
| `source` | string | dbt `unique_id`, e.g. `source.ingestion.bronze_jira.jira_issue`. |
| `status` | string | `warn`, `error`, or `runtime error`. |
| `max_loaded_at` | string \| `"(table is empty)"` | ISO-8601, or sentinel literal when `empty=true`. |
| `age_hours` | number \| null | `null` when `empty=true` or when dbt didn't emit `max_loaded_at_time_ago_in_s`. |
| `empty` | boolean | `true` when `MAX(...)` resolved to the `1970-01-01` sentinel. |

#### 3.6.2 Per-driver mapping of the canonical payload

| Driver | Payload kind | Markdown | What carries the breach list |
|---|---|---|---|
| `webhook` | JSON, full canonical shape | none (raw JSON) | `breaches[]` |
| `zulip` | form-urlencoded `text` field | Slack mrkdwn (`*bold*`, `\``code`\``, `•`) | `text` (newline-joined lines) |
| `slack` | JSON `{text, channel?}` | GitHub-style mrkdwn (`**bold**`, `\``code`\``, `-` bullets) | `text` (newline-joined) |
| `teams` | JSON MessageCard | GitHub-style mrkdwn | `text` (paragraph-joined) |
| `email` | text/plain | none | message body |

#### 3.6.3 Failure semantics

- Notification failure raises inside the dispatcher and is caught at
  workflow template line 550, printed to stderr, and **does not
  change** the workflow's primary exit code (lines 551–554).
- Page decision (workflow template line 559): `sys.exit(1)` if any
  breach has `status ∈ {error, runtime error}`, else `0`.

### 3.7 Failure Semantics

Parser exit codes (set inline in the workflow template):

| Code | Meaning | Source line |
|---|---|---|
| `0` | All sources within SLA, OR only `warn` breaches (visible in log + payload, not page-worthy) | line 364 / 559 |
| `1` | At least one `error` or `runtime error` (page-worthy) | line 559 |
| `2` | `target/sources.json` missing — dbt crashed before producing a report | line 323 |

The shell wraps the parser in `set +e`, captures `freshness_exit=$?`,
runs the trap detector, then `exit "$freshness_exit"` (workflow template
lines 561–581). Trap detector's exit (`trap_exit`) only writes a notice
and never overrides `freshness_exit`.

### 3.8 Helm Surface

Sourced verbatim from
[`charts/insight/values.yaml:147-292`](../../../../charts/insight/values.yaml).
Schema: [`charts/insight/values.schema.json:26-99`](../../../../charts/insight/values.schema.json).

| Helm path | Type | Default | Purpose |
|---|---|---|---|
| `ingestion.freshness.enabled` | bool | `true` | Top-level kill switch. |
| `ingestion.freshness.schedule` | string | `"0 13 * * *"` | Cron — sits past every connector sync window (02:00–11:00 UTC) plus 2 h grace. |
| `ingestion.freshness.dbtSelect` | string | `"source:*"` | Default selector. Narrowed at runtime to deployed bronze databases. |
| `ingestion.freshness.cluster` | string | `""` | Identity label — installation tier. |
| `ingestion.freshness.tenant` | string | `""` | Identity label — customer / workspace. |
| `ingestion.freshness.thresholds.defaultWarnHours` | int | `30` | "default" tier warn. |
| `ingestion.freshness.thresholds.defaultErrorHours` | int | `48` | "default" tier error. |
| `ingestion.freshness.thresholds.eventWarnHours` | int | `72` | "event" tier warn. |
| `ingestion.freshness.thresholds.eventErrorHours` | int | `96` | "event" tier error. |
| `ingestion.freshness.thresholds.reportWarnHours` | int | `48` | "report" tier warn. |
| `ingestion.freshness.thresholds.reportErrorHours` | int | `96` | "report" tier error. |
| `ingestion.freshness.thresholds.reportExtendedWarnHours` | int | `72` | "report_extended" tier warn. |
| `ingestion.freshness.thresholds.reportExtendedErrorHours` | int | `120` | "report_extended" tier error. |
| `ingestion.freshness.notification.driver` | enum | `""` | One of `""`, `webhook`, `zulip`, `slack`, `teams`, `email`. Schema-validated. |
| `ingestion.freshness.notification.webhook.urlSecret.{name,key}` | secretRef | — | Required when driver=webhook. |
| `ingestion.freshness.notification.zulip.urlSecret.{name,key}` | secretRef | — | Required when driver=zulip. |
| `ingestion.freshness.notification.zulip.stream` | string | `""` | Optional override; else inherits from URL query. |
| `ingestion.freshness.notification.zulip.topic` | string | `""` | Optional override. |
| `ingestion.freshness.notification.slack.urlSecret.{name,key}` | secretRef | — | Required when driver=slack. |
| `ingestion.freshness.notification.slack.channel` | string | `""` | Optional channel override. |
| `ingestion.freshness.notification.teams.urlSecret.{name,key}` | secretRef | — | Required when driver=teams. |
| `ingestion.freshness.notification.email.smtp.host` | string | `""` | Required when driver=email. |
| `ingestion.freshness.notification.email.smtp.port` | int | `587` | 587 STARTTLS / 465 SMTPS / 25 plain. |
| `ingestion.freshness.notification.email.smtp.username` | string | `""` | Optional SMTP AUTH user. |
| `ingestion.freshness.notification.email.smtp.passwordSecret.{name,key}` | secretRef | — | SMTP AUTH password (rendered only when set). |
| `ingestion.freshness.notification.email.smtp.starttls` | bool | `true` | Ignored when port=465. |
| `ingestion.freshness.notification.email.from` | string | `""` | Required when driver=email. |
| `ingestion.freshness.notification.email.to` | string | `""` | Required when driver=email — comma-separated. |
| `ingestion.freshness.notification.email.subjectPrefix` | string | `"[ingestion-freshness]"` | Prefix prepended to `summary`. |

## 4. Decisions / Why we did X

### 4.1 Why two report tiers (`report`, `report_extended`) instead of one

Microsoft Graph reports document a 24–48 h publish lag — sliding the
warn to 48 h is just "the upper edge of stated normal". Slack
admin.analytics is observed and documented to lag ~3 days, with
stretches up to 5 days during Slack maintenance — applying the
report-tier (96 h error) to Slack would page on every Slack maintenance
window. They needed different bands, but lumping every 3-day vendor
into "event" would lose the semantic signal that `event` is for natural
quiet days, not for vendor lag.

### 4.2 Why `_airbyte_extracted_at` for streaming and a business date for re-emit

Streaming connectors land rows in bronze approximately when they
happen — `_airbyte_extracted_at` tracks reality. Report-style
connectors re-fetch a fixed window every run: even when the upstream
stops publishing, Airbyte still writes "fresh" rows for older business
days, so `_airbyte_extracted_at` keeps advancing forever. The
`feature-bronze-freshness-sla/FEATURE.md §2.1` example (M365 9 h fresh
extracted-at vs 96 h-stale `reportRefreshDate` on 2026-05-04) is the
canonical evidence. Anchoring on the business-date column flips the
verdict to ERROR, which is correct.

### 4.3 Why per-driver subblocks instead of a flat `notification.url`

Different drivers need different *non-credential* settings (Zulip
stream/topic, Slack channel, SMTP host/port/from/to/subjectPrefix). A
flat `notification.url` would force these into either the URL itself
(Zulip's `?stream=...` works; Slack's channel override does not always)
or into an out-of-band lookup table. Per-driver subblocks let each
driver own its shape; the dispatcher just reads the env vars it cares
about. Adding a sixth driver is one Helm subblock + one match arm + one
`secretKeyRef` branch — no other Helm surface changes (workflow
template line 380–382, `values.yaml:195-201`).

### 4.4 Why the Zulip dispatcher silently rewrites the endpoint

Zulip exposes two incoming-webhook integrations under the same UI
("Incoming webhook"): `/api/v1/external/json` (intended for debugging,
dumps body as JSON code block) and `/api/v1/external/slack`
(Slack-compatible, renders mrkdwn). The UI does not warn that the
former is the wrong integration for routine notifications. Rather than
require operators to know the Zulip-internal distinction, the
dispatcher rewrites the path on the way out (workflow template line
457). This is documented in the chart values comment
(`values.yaml:230-239`) and in the dispatcher's docstring (lines
445–478).

### 4.5 Why narrow the selector at runtime

`source:*` against a partial deployment (e.g. a tenant who hasn't
deployed OpenAI) would emit one ERROR per missing source on every run,
flooding on-call with noise about something they deliberately didn't
deploy. Narrowing to `bronze_*` databases that exist in
`system.databases` (workflow template lines 262–297) keeps the check
scoped to "what we collect", and explicitly leaves "what should be
collected" to the deployment-health workstream
([#272](https://github.com/cyberfabric/insight/issues/272)).

### 4.6 Why the trap detector is advisory

The freshness check is the page-worthy signal — its threshold tiers are
documented per source, its semantics are deterministic, its output is
the canonical payload. The trap detector is heuristic (95 % / 2 days /
100 rows are tunable thresholds) plus an opt-in. False positives on a
heuristic that pages on-call would erode trust in the whole monitoring
domain. So the trap detector logs, the freshness parser owns the
verdict (workflow template lines 575–579).

### 4.7 Why `meta.freshness_optout_reason` is mandatory

Bare `freshness: null` is too easy to leave in by accident. Once it
lands, the table is invisible to monitoring forever, with no comment
explaining why. Forcing a one-line rationale at the time of writing
(`lint-bronze-freshness.py:88-97`) keeps the audit surface
grep-friendly: `grep -A1 'freshness: null'` shows what every opt-out
is for.

## 5. Traceability

- Feature spec: [`feature-bronze-freshness-sla/FEATURE.md`](feature-bronze-freshness-sla/FEATURE.md)
- Operator runbook: [`src/ingestion/MONITORING.md`](../../../../src/ingestion/MONITORING.md)
- Implementation files (`feat/bronze-freshness-sla` branch in `dzarlax/insight`):
  - Workflow + parser: [`charts/insight/templates/ingestion/dbt-source-freshness.yaml`](../../../../charts/insight/templates/ingestion/dbt-source-freshness.yaml)
  - Helm surface: [`charts/insight/values.yaml`](../../../../charts/insight/values.yaml) lines 147–292
  - Schema: [`charts/insight/values.schema.json`](../../../../charts/insight/values.schema.json) lines 26–99
  - dbt project freshness defaults: [`src/ingestion/dbt/dbt_project.yml`](../../../../src/ingestion/dbt/dbt_project.yml) lines 70–79
  - CI lint: [`src/ingestion/scripts/lint-bronze-freshness.py`](../../../../src/ingestion/scripts/lint-bronze-freshness.py)
  - Trap detector: [`src/ingestion/scripts/freshness-trap-detect.py`](../../../../src/ingestion/scripts/freshness-trap-detect.py)
  - Per-connector schemas: `src/ingestion/connectors/*/*/dbt/schema.yml`
- Relevant commits on `feat/bronze-freshness-sla`:
  - `542756f` — driver-based notification (replaces flat `notificationWebhookUrl` with per-driver subblocks).
  - `c196fdb` — Zulip render fix (`/external/json` → `/external/slack` rewrite + Slack-compatible form fields), explicit `User-Agent` for Cloudflare-fronted receivers, ZWSP note in Zulip stream values.
  - `23d1b18` — empty-table sentinel (1970-01-01 detection in the parser).
- Future work cross-references:
  - Deployment health — [issue #272](https://github.com/cyberfabric/insight/issues/272).
  - Volume baseline / source-vs-bronze attribution / ownership — "Open work" sections of [`MONITORING.md`](../../../../src/ingestion/MONITORING.md).
