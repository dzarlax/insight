# Ingestion Monitoring — operator runbook

Goal: detect "data is missing for several days" before users notice. Today's
coverage is **freshness only** — newly arrived rows in bronze tables. Volume
and Airbyte-job-level signals are listed under [Open work](#open-work).

This file is the operator-facing runbook (verification steps, on-call
matrix, parser exit codes, payload shape). The feature design itself —
purpose, threshold inheritance, acceptance criteria — lives in
[`docs/domain/ingestion/specs/feature-bronze-freshness-sla/FEATURE.md`](../../docs/domain/ingestion/specs/feature-bronze-freshness-sla/FEATURE.md).

## What's wired

### Bronze freshness (live)

Every Airbyte-managed bronze source inherits a 30h-warn / 48h-error
threshold against `_airbyte_extracted_at`. The threshold is defined once at
project level in `src/ingestion/dbt/dbt_project.yml`:

```yaml
sources:
  ingestion:
    +freshness:
      warn_after:  { count: 30, period: hour }
      error_after: { count: 48, period: hour }
```

Each source's `schema.yml` then declares the field to check:

```yaml
sources:
  - name: bronze_<connector>
    schema: bronze_<connector>
    loaded_at_field: _airbyte_extracted_at
    tables: ...
```

`loaded_at_field` is a dbt **property**, not a config — `+loaded_at_field`
at project level is silently ignored. The dbt-clickhouse adapter does not
support metadata-based freshness, so a source missing `loaded_at_field`
fails with `runtime error` instead of falling back to a default.

A single CronWorkflow `dbt-source-freshness-check` runs `dbt source freshness`
daily at 13:00 UTC (after every connector's sync window of 02:00–11:00 UTC)
and parses `target/sources.json`. Any source in `warn` or `error` is logged
with name, max-loaded-at and lag, and (if a webhook URL is configured) the
list is POSTed as JSON to a notification channel.

| Status | Meaning | Workflow exit | What to do |
|--------|---------|---------------|------------|
| `pass` | MAX(`_airbyte_extracted_at`) within last 30h | 0 | Nothing |
| `warn` | 30–48h lag (one missed run) | 0 (visible in log + payload) | Investigate during business hours |
| `error` | >48h lag (multiple missed runs) | 1 (Argo Failed) | Page |
| `runtime error` | dbt couldn't even check the source (CH down, schema drift, query failure) | 1 (Argo Failed) | Page — investigate before trusting other sources |

`error` and `runtime error` flip the workflow to Failed so Argo retains the
run in `failedJobsHistoryLimit`. Warn-only runs stay Successful — the breach
is still printed to the workflow log and POSTed in the notification payload,
but on-call doesn't get paged on a single missed sync.

### How it stays generic

New connectors **do not** need to repeat the freshness block. They get the
default the moment they declare a `bronze_*` source under
`src/ingestion/connectors/.../dbt/schema.yml`. Two knobs the connector author
controls:

1. **Per-table opt-out** — slow-moving lookup/catalog streams (e.g.,
   `jira_statuses`, `claude_admin_workspaces`, `bamboohr.meta_fields`) set
   `freshness: null` next to the table in `schema.yml`. A quiet day is
   legitimate for those.
2. **Tighter SLA** — sub-daily connectors (none today) override at the source
   level in `schema.yml` with their own `freshness:` block.

That's it. No per-connector pipeline plumbing.

## Who consumes the signal

Per-environment ownership matrix until the delivery channel is wired (see
[Open work](#open-work)).

Ownership for "ingestion on-call" and "connector owner" is not assigned yet
(no rotation document, no `CODEOWNERS` for `src/ingestion/connectors/*` as
of this commit). The matrix below describes the *roles* the freshness
signal expects to land on; see [Open work](#open-work) for the rotation gap.

| Role | What they read | When | Action |
|---|---|---|---|
| Ingestion on-call (TBD) | Argo UI / `kubectl get workflows -n argo --sort-by=.metadata.creationTimestamp` for the `dbt-source-freshness-check` runs | Daily, after the 13:00 UTC run | Triage `error` / `runtime error` runs |
| `cyberfabric/insight` repo Issues | One issue per persistent breach (>2 consecutive runs) opened by the on-call | Within 1 business day of the breach | Hand off to the connector owner |
| Connector owner (TBD per connector) | The issue body — includes the failing source, max-loaded-at, lag in hours | On issue assignment | Fix the connector or update the SLA |
| Tenant on-call (post-MVP) | Webhook payload (Zulip / email / generic POST) routed by `cluster` field | Real-time | Same triage as above, scoped to one deployment |

Until the webhook channel lands, the **only** push mechanism is Argo's
failed-runs list — on-call must check `kubectl get workflows -n argo
--sort-by=.metadata.creationTimestamp` at least once per business day. The
`failedJobsHistoryLimit: 5` ensures the latest five breaching runs are
retained for inspection.

`pass`-only runs leave no trace beyond Argo's success history (kept by
`successfulJobsHistoryLimit: 3`) — silent green is the desired steady state.

## Open work

### Rotation / ownership — not assigned

The matrix above describes roles, not people. There is no documented
ingestion on-call rotation as of this commit, and `src/ingestion/connectors/`
has no `CODEOWNERS` entries assigning per-connector owners. Until that
lands, the freshness signal lives in Argo's failed-runs list with no
named consumer.

Action: agree on an on-call rotation (or a single owner during MVP) and
add a `CODEOWNERS` block listing the per-connector owner so the breach
hand-off has a real target.

### Delivery channel — to be decided

`charts/insight/values.yaml` exposes
`ingestion.freshness.notificationWebhookUrl`. Default is empty, so today
breaches surface only via:

- Argo UI / `kubectl get workflows -n argo -l app.kubernetes.io/component=ingestion-monitoring`
- Workflow exit status (failed runs accumulate in
  `failedJobsHistoryLimit: 5`)

We need a pull/push channel. Slack is not on the table. Candidates:

| Option | Pros | Cons |
|--------|------|------|
| Zulip incoming webhook | We already use Zulip; one URL, native topics | Needs a bot, per-tenant channel mapping |
| Email via the platform mailer | Reuse the existing notification-email path used by Backend alerts (see `docs/components/backend/specs/PRD.md` §5.6) | Latency, no threading |
| Generic webhook → small relay (Cloud Function / k8s service) | Decouples delivery from connector pipeline | Extra moving part |
| Argo `onExit` notification (e.g. `argoproj-labs/argo-notifications`) | First-class Argo support, retries, templating | Heavier setup |

The workflow's POST body is intentionally **provider-agnostic** so we can
swap any of these in without touching the connector code:

```json
{
  "topic": "ingestion-freshness",
  "cluster": "prod",
  "summary": "[prod] 3 bronze source(s) breaching freshness SLA",
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

`cluster` is set from `ingestion.freshness.cluster` in values overrides. It
identifies the deployment so receivers can route between staging/prod or
per-tenant channels. Empty string is allowed for single-deployment installs;
the `summary` then drops the `[…]` prefix.

A Zulip incoming webhook expects `content` + topic in the URL — we'll need a
2-line transformer if we go that route. Decision: TODO.

### Volume baseline (next iteration)

Freshness catches "no rows arrived" but misses "API returned 50 rows instead
of 5000". Plan: a singular SQL test (or dbt operation) that compares today's
row count per stream against a 14-day median, alerts on <30%. Reuses the same
`dbt-source-freshness` workflow shell — just a different selector.

### Source vs bronze attribution

Today the freshness check flags "no rows in bronze" but cannot tell whether
the upstream Airbyte sync ran. To distinguish:

- *source/credential issue*: Airbyte sync ✅, bronze pulled 0 rows
- *pipeline issue*: Airbyte sync ❌ (didn't run / errored)

We'd need a sidecar that polls the Airbyte Jobs API after each sync and
appends a row to `staging.airbyte_runs (connection_id, status,
records_emitted, started_at, ended_at)`. Then the freshness step can JOIN
against that table and label each breach with a root cause. Future PR.

## How to verify locally

```bash
# After dev-up.sh + at least one successful sync
kubectl create -n argo -f - <<'EOF'
apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata:
  generateName: freshness-adhoc-
  namespace: argo
spec:
  workflowTemplateRef:
    name: dbt-source-freshness
  arguments:
    parameters:
      - name: dbt_select
        value: "source:bronze_jira"
      - name: toolbox_image
        value: "insight-toolbox:local"
      - name: clickhouse_host
        value: "insight-clickhouse.insight.svc.cluster.local"
      - name: clickhouse_port
        value: "8123"
      - name: clickhouse_user
        value: "default"
EOF

# Watch the run
kubectl logs -n argo -l workflows.argoproj.io/workflow=<name> -f
```
