# Engineering PRD: Analytics Diagnosis Layer

## 0. TL;DR (one-pager)

> Read this first. The full doc is the deep-dive. If you only have 5 minutes, read this section and §14.2 (track overview), and stop.

**Problem.** Insight has 130+ widgets. Managers don't read them. The product answers "how many?" — there's no layer between raw data and managerial action.

**What we're building.** A diagnosis layer that turns dashboards from passive into active: rules evaluate metrics against baselines, return a per-cohort verdict (Green / Yellow / Red / Partial), and surface flags with explanations and suggested actions. Existing widgets become the drill-down, not the entry point.

**Three building blocks.**

1. **Rule-Flag engine** (§3.2) — strict `RuleAST` with cohort filter + metric + operator + threshold. Threshold values come from a Metric Catalog (single source of truth, no `Literal`s in code). Rules can be `Global` (Constructor-seeded), `Tenant`, or `Team`.
2. **Cohorts via two-axis model** (Identity §4.3) — `expected_functions` (set, tenant-declared, multi-membership) ⊥ `observed_activity` (per-function vocabulary). Mismatch is a first-class signal: PM-who-codes, sales-rep-with-no-deals, etc.
3. **Verdict + action ladder** (§5, §5.1) — verdict on the dashboard banner; flag click drives drill-down filtering; each rule carries a suggested-action template so the manager knows what to do, not just what's wrong.

**Phasing at a glance.**

```text
F0a (skeleton, literal threshold)  →  F0b (catalog MVP)  →  F1 (deterministic MVP)
                                                              ↓
F2 (verdict history)  →  F3 (relative rules, single tenant)
                                                              ↓
F4a (HR cohorts, multi-tenant, self-install wizard)  →  F5 (LLM profiles)  →  F6 (NL→Rule Copilot)  →  F7 (push)
```

Cut order under pressure: F7 → F6 → F5 → F4a → F3. Never cut F0a/F0b/F1 — they are the product.

**What we are explicitly NOT building** (§14.8). Numeric performance scores. IC-visible flags about themselves. Cross-tenant industry benchmarks. Paging on Critical. A role taxonomy of our own (we consume person-domain). Connectors beyond what's already on the connector roadmap.

**Implementation reality check (May 2026 audit).**

| Foundation we depend on | Status in code |
|---|---|
| identity-resolution (`persons`, `aliases`) | Tables exist; only email / employee_id / display_name aliases emitted today; github / bitbucket aliases not emitted; Slack resolution works via `users_details.email_address` because Slack admin API returns no user roster or channel data (counters only) |
| metric-catalog | PRD-only, **no code, no open PR**; F0b absorbs the minimum-viable slice |
| org-chart (`person_assignments`) | PRD-only, no code, no owner; F4a is hard-blocked on it |
| person-domain (override API) | Folded into identity; manual override = dbt seed PR (not an API) |
| connector custom-fields | Config flag exists, schema is flat (no `Map(String, String)`); I-E.1 is real work |
| tenant isolation | Skipped in `analytics-api/handlers.rs:287` AND `query_metric()`; F0 mandates audit pass |

**What this means.** Several phases assume foundations that need ownership escalated *before* their kickoff, not during. F0a is the only phase that can start with what's in code today.

**Reading guide.**

*   **PM / leadership**: §0 (this), §13 (success metrics), §14.6 (cut order), §14.8 (what we're not building), §16 (open questions).
*   **Engineering reviewers**: §0, §3 (building blocks), §4 (foundations), §10 (API contracts), §15 (risks).
*   **Identity / cohort details**: companion doc [`IDENTITY_AND_COHORTS.md`](./IDENTITY_AND_COHORTS.md) — start with its §0 (audit) and §4.3 (two-axis model).
*   **Phasing details and dependencies**: §14.

---

## 1. Overview

This document translates the product vision of the Analytics Diagnosis Layer into concrete engineering requirements. It maps the product capabilities directly to the existing Constructor Insight architecture, detailing the technical decisions, data flows, and specific changes needed in the ingestion, backend, and frontend codebases.

### 1.1 Background & Problem Statement
Currently, Constructor Insight provides over 130+ metric widgets across various integrations (Jira, Zoom, Slack, M365, etc.). It excels at answering "How many?" (e.g., how many commits, how many meeting hours). However, this creates massive cognitive overload for managers. A Team Lead has to manually review dozens of charts, correlate anomalies across tabs, and subjectively decide if their team is performing well or burning out. 

As a result, managers rarely use the tool daily. The core problem is that **there is no intermediate layer between raw data and managerial decisions that performs the work of interpretation**. 

### 1.2 Business Outcome (The "Why")
The Analytics Diagnosis Layer solves this by transforming Insight from a passive "dashboard" into an **active diagnostic tool**. 
Instead of forcing a manager to hunt for anomalies, the system automatically evaluates the data against predefined baselines and rules, returning a simple verdict: `All Good / Needs Attention / Critical`. The existing 130+ charts do not disappear; rather, they become the "drill-down" layer—the place a manager goes only *after* the Diagnosis Layer points them to a specific problem.

---

## 2. Principle of Operation & System Architecture

**How does the Analytics Diagnosis Layer work end-to-end?**

The system operates as a continuous, multi-layered pipeline:

1. **Data Ingestion & Normalization (`insight/src/ingestion`)**: 
   Airbyte pulls raw events from SaaS tools (Jira, Zoom) and HR systems. `dbt` processes this data to build the **Identity Layer** (mapping individuals to a `job_role` and team) and the **Metric Catalog** (clean, aggregated facts).
2. **Baseline Generation (Defining "Normal")**:
   `dbt` calculates an "Active Fingerprint" (e.g., P25/P50/P75 metrics) for every role across a tenant. A Rust background worker sends this fingerprint to the internal LLM Gateway to generate a human-readable **Role Profile draft**. Once a Tenant Admin approves it, this profile becomes the active baseline.
3. **Deterministic Rule Evaluation (`insight/src/backend/services/analytics-api`)**:
   When a dashboard is requested, the backend evaluates predefined **Rules** (e.g., "commits < 20th percentile of the role") against the normalized data. 
   *Crucial Step*: Before evaluating, the engine checks the **Data Health State**. If a source (e.g., Zoom) failed to sync, dependent rules are skipped, and the UI degrades gracefully.
4. **Verdict & Drill-down (`insight-front/src/app`)**:
   Matched rules aggregate into a final **Verdict** (Green/Yellow/Red). The React SPA renders this as a banner. Clicking a flag dispatches a state update that filters existing charts to show exactly who triggered the rule.
    A manager requests a custom rule in natural language. The backend injects the Metric Catalog schema into the LLM, validates the generated Rule AST, runs a dry-run preview, and saves the new rule to the database.

### 2.1 Architectural Boundaries — what this PRD owns and what it doesn't

The Analytics Diagnosis Layer sits on top of several existing Insight domains. Responsibilities are strictly separated and the canonical specs win on contract questions.

**Implementation reality (May 2026 code audit) before reading this table.** The dependencies below are described at the *contract* level — what each domain *will own* per its PRD. Code reality is different and is captured in [Identity & Cohorts §0](./IDENTITY_AND_COHORTS.md#0-relationship-to-existing-domain-specs): metric-catalog has no code, org-chart has no code, person-domain has no service (dbt seeds only), connector custom-fields land flat (no Map column). The diagnosis layer's plan must consume these *contracts as they ship*, not as they were *promised to ship*. Phasing in §14.3 and risks in §15 reflect this; this table is the contract intent, not a status report.

| Domain | Spec | Owns | Diagnosis layer's relationship |
|---|---|---|---|
| Metric Catalog | `insight/docs/domain/metric-catalog/specs/PRD.md` (merged v1) | `metric_catalog(metric_key, label_i18n_key, unit, format, higher_is_better, source_tags, is_enabled, ...)`. `metric_threshold(metric_key, scope, role_slug, team_id, good, warn, alert_trigger, alert_bad, is_locked, lock_reason, ...)`. Scope ∈ `{product-default, tenant, role, team, team+role}`. Resolution chain `team+role → team → role → tenant → product-default`, **locks are ceilings** (resolution stops). `tenant_id IS NULL` enforced by CHECK in v1. Migration-only metadata writes. | **Consumer.** Every rule's `RuleAST` references a valid `metric_key`. Threshold values come from `GET /catalog/metrics?role_slug=...&team_id=...` — diagnosis layer **does not reimplement resolution**. Verdict's `explanation` exposes `resolved_from` so admins see which scope supplied the threshold. Locks are respected: a rule cannot override a locked tenant threshold with a narrower-scope value (§7.4 below). Calculation rules / `primary_query_id` are deferred in catalog v1 — our engine still derives "how is metric X computed" from `query_ref` directly. `kind` enum (visual vs alerting vs diagnosis) is DESIGN-owned in catalog; we likely register as `kind=diagnosis` or reuse `alert` (Open Q §16.9). |
| Identity Resolution | `insight/docs/domain/identity-resolution/specs/PRD.md` | `aliases`, bootstrap pipeline, alias→`person_id` hot-path lookup. | **Consumer.** Diagnosis uses resolved `person_id`. **Extends** with author/reviewer namespace bridge (Identity doc §2.2; tracked as I-D in §14.4) — out of IR's current scope. |
| Person | `insight/docs/domain/person/specs/PRD.md` | `persons` golden record, manual overrides via `*_source = 'manual'`, person availability. | **Consumer.** Reads `role`, `manager_person_id`, `org_unit_id`. Tenant-admin overrides flow through this domain's mechanism — no parallel override table. |
| Org Chart | `insight/docs/domain/org-chart/specs/PRD.md` | `org_units` (SCD Type 2), `person_assignments` (temporal `assignment_type` ∈ `{org_unit, role, department, team, manager, project, location, cost_center}`). | **Consumer + extension.** Reuses `person_assignments` for slot historization. May require **new `assignment_type` values** for slots like `product`, `subteam`, plus a generic `function_signal_binding` carrier (per Identity §3.1.1) — coordinated with org-chart team. |
| Connector Framework | `insight/docs/domain/connector/specs/PRD.md` | Bronze ingestion. §5.2 already requires support for source-specific custom fields (BambooHR custom attributes) without core schema change. | **Consumer.** I-E.1 (BambooHR custom-field expansion in §14.4) is **not** a new requirement — it's an existing contract. Diagnosis adds the slot-mapping layer (Identity doc §3.2) on top. |

The full identity & cohort contract — including how slot mapping bridges tenant-specific source fields to org-chart's `assignment_type`, and how eligibility predicates compose org-chart slots with activity signals — lives in the [Identity & Cohorts companion doc](./IDENTITY_AND_COHORTS.md), with its own §0 reconciliation table.

## 3. The Four Core Building Blocks (Technical Deep Dive)

The product defines four necessary "building blocks". Here is how they must be engineered.

### 3.1 Derived Metrics Engine
*   **What**: A system capable of computing ratios, deltas (QoQ, MoM), and rolling averages on top of base metrics.
*   **Why**: Hardcoded absolute thresholds ("< 10 commits") rot quickly. Dynamic thresholds require relative metrics.
*   **How**:
    *   **Backend (`analytics-api`)**: Implement a SQL query builder that can compose complex aggregations dynamically.
    *   **Ingestion (`dbt`)**: For very heavy moving averages (e.g., "4-week rolling average of meeting hours"), build materialized views or incremental models in `dbt` (`silver` layer) to pre-calculate these for fast querying.

### 3.2 Rule-Flag Engine (The AST)
*   **What**: The execution engine that evaluates named conditions against specific cohorts.
*   **Why**: To provide a deterministic, verifiable way to flag anomalies without writing raw SQL.
*   **How**:
    *   **Backend (`analytics-api`)**: Define a strict Rust `struct RuleAST`.
        ```rust
        pub struct RuleAST {
            pub cohort_filter: CohortFilter,   // role_slug, team_id, predicate refs (Identity doc §4.3)
            pub metric_key: String,            // must exist in metric_catalog and be is_enabled=true
            pub operator: Operator,            // <, >, <=, >=, ==
            pub threshold: ThresholdSource,    // see below
            pub scope: RuleScope,              // Team, Tenant, Global — rule provisioning scope, NOT threshold scope
        }

        pub enum ThresholdSource {
            // ABSOLUTE numerical thresholds — exclusively from metric-catalog.
            // Resolved at evaluation time using the cohort's (tenant, role, team) context.
            Catalog { kind: ThresholdKind },             // ThresholdKind ∈ {good, warn, alert_trigger, alert_bad}

            // RELATIVE / DERIVED — not absolute thresholds; computed from baselines or history.
            // The catalog explicitly does not own these.
            Percentile { p: u8 },                         // 1..=99 — vs dbt-baseline percentile of cohort
            RelativeDelta { period: Period, pct: f64 },   // e.g. -30% MoM — vs verdict_history
        }
        ```
    *   **Single source of truth for absolute thresholds.** No `Literal(f64)` variant. If a rule wants to fire on an absolute value (e.g. "pr_cycle_time_h > 72"), the value lives in `metric_threshold` and the rule references it via `Catalog { kind: warn }`. Tenant admins edit once in the catalog and diagnosis adapts automatically.
    *   The engine translates this AST into an OLAP SQL query. For `Catalog` thresholds, the engine **does not reimplement resolution** — it calls the catalog's `GET /catalog/metrics?role_slug=...&team_id=...` and uses the returned threshold + `resolved_from`. For `Percentile`, we rely on `dbt`-generated role baselines (calculating `PERCENTILE_CONT` dynamically per role at query time is too expensive). For `RelativeDelta`, we read from `verdict_history` (main F2).
    *   **Catalog-vs-rule separation**: the catalog says *what value is good/warn/bad*; our rule says *under what cohort and direction* this matters. They compose: a rule with `ThresholdSource::Catalog { kind: warn }` and `Operator::>` fires when the metric exceeds the catalog-resolved `warn` threshold for the cohort.
    *   **F1 dependency on catalog-seeding.** Every seeded `Catalog`-sourced rule requires a corresponding `product-default` threshold row in `metric_threshold`. F1 is **gated on coordinated metric-catalog seeding** for the 5 seeded rules' metrics. If the catalog has no row at any scope for a referenced metric, the rule resolves `not_applicable` (honest-NULL — main §4.2) and reports the gap in `partial_reasons`.

### 3.2.1 Rule Provisioning & Scope (Where do rules come from?)
*   **What**: The system requires a populated library of rules to function. The rules originate from three distinct levels of scope.
*   **Why**: A system without rules on Day 1 is useless. We must provide immediate value while allowing local customization.
*   **How**:
    *   **Base Rules (`scope = Global`)**: The backend must run migrations to seed ~5 default rules built by Constructor (e.g., "low commits", "high meeting load"). These apply to all tenants automatically but can be toggled off.
    *   **Custom Rules (`scope = Tenant`)**: Rules created by Tenant Admins that apply across their entire company.
    *   **Local Rules (`scope = Team`)**: Rules generated via Chat-Copilot by team leads. The database must strictly enforce that these rules are only visible to and evaluated against the specific `team_id`.

### 3.3 Chat-Copilot (NL-to-Rule API)
*   **What**: The interface converting manager text into valid `RuleAST` objects.
*   **Why**: Managers cannot write ASTs or SQL. The LLM bridges this gap but must not introduce hallucinations into the actual metrics.
*   **How (API Mapping based on Anna's Scenario)**:
    1.  `POST /api/v1/copilot/chat`: Frontend sends `"find people with chat activity dropped by 30%"`.
    2.  Backend injects the Metric Catalog schema and the user's `team_id` context into the prompt, sending it to the Internal LLM Gateway.
    3.  Backend parses the LLM response into a `RuleAST`. If validation fails, it asks the LLM to correct itself (max 2 retries; after that, return a structured error to the FE so the user can rephrase).
    4.  `POST /api/v1/rules/preview`: Backend runs a `dry-run` SQL query using the `RuleAST`. Returns `{"matched_users": ["user_1", "user_2"], "total_cohort": 10}`.
    5.  Frontend displays the preview.
    6.  `POST /api/v1/rules`: Manager confirms, and the rule is saved with `scope = Team`.

#### 3.3.1 LLM Safety, Validation & Cost Controls
*   **AST validation contract**: The LLM is constrained to emit JSON conforming to a strict schema. Backend validates:
    *   `metric_key` exists in `metric_catalog` and `is_enabled = true` (rejects hallucinated keys and disabled metrics).
    *   `operator` ∈ allowlist `{<, >, <=, >=, ==}`.
    *   `threshold` matches one of `ThresholdSource` variants (§3.2 — `Catalog`, `Percentile`, `RelativeDelta`; no `Literal`). For `Percentile`, `p` ∈ `[1, 99]`; for `RelativeDelta`, `pct` ∈ `[-100, 1000]`. For `Catalog`, `kind` ∈ `{good, warn, alert_trigger, alert_bad}` AND a catalog row exists at some scope for the caller's tenant (rejects rules that would always resolve to `not_applicable`; locks are honoured but not rejected).
    *   `cohort_filter.team_id` is forced server-side to the caller's team (the LLM **cannot** set it). Cross-tenant filters are stripped before execution. `cohort_filter.role_slug` and predicate references are validated against the catalog's known string values.
*   **Prompt-injection defense**: User text is wrapped in delimited blocks; the system prompt instructs the model to treat it as data, not instructions. The Metric Catalog injected into the prompt is filtered to the caller's tenant. Output is parsed as JSON only — free-form completions are rejected.
*   **Rate limiting & cost budget**: Per-tenant daily token budget configured in `analytics-api`. When exceeded, `/copilot/chat` returns `429` with a retry-after; the Admin Rule Builder (§3.5) remains usable as a non-LLM fallback.
*   **Fallback when LLM is down**: `/copilot/chat` returns `503` with a banner directing users to the Admin Rule Builder. `/rules/preview` and `/rules` (deterministic paths) remain available.
*   **Eval harness**: A golden set of ≥40 NL→AST examples lives in `insight/src/backend/services/analytics-api/tests/copilot_golden.json` and runs on every model bump and prompt change. Regressions block deploys.
*   **Model versioning**: The active model id and prompt version are persisted alongside each generated rule (`source_model`, `source_prompt_version`) for auditability and reproducibility.

### 3.4 Auto-Profiles Pipeline (Role Definition & Description)
*   **What**: Generation and review queue for role-based expected metric ranges.
*   **Why**: Manual configuration of 70+ roles per enterprise client is unscalable.
*   **How Roles are Defined**: Roles come from the person/identity domain — `person.persons.role` field, populated from HR. Today raw text; once identity-domain normalization lands (where `role_alias`-style mapping lives), roles become stable slugs. Diagnosis layer reads whatever the field holds and treats it as a string.
*   **How Roles are Described (Sequence Mapping)**:
    1.  **Nightly Cron (`dbt`)**: Runs `model_role_fingerprints.sql` to calculate P25, P50, P75 for core metrics per `job_role`.
    2.  **Nightly Worker (`analytics-api`)**: Iterates over roles, sending the statistical fingerprint + HR Role Title to the LLM to generate a human-readable description and expected focus areas.
    3.  **Database**: Saves to `role_profiles` table with `status = 'pending_review'`.
    4.  **Frontend (`insight-front`)**: Tenant Admin opens the Queue UI. Fetches `GET /api/v1/role-profiles?status=pending_review`.
    5.  Admin reviews the LLM-generated description and baseline diff. Submits `PUT /api/v1/role-profiles/{id}` with `status = 'active'`.

### 3.5 Admin Rule Builder UI
*   **What**: A visual interface in the Admin Panel for constructing `RuleAST` objects.
*   **Why**: While Chat-Copilot is great for Team Leads, Tenant Admins (Compliance, HR, Ops) need a deterministic, precise UI to author company-wide (`scope = Tenant`) rules without relying on LLM interpretation.
*   **How**:
    *   **Frontend (`insight-front`)**: Build a visual query builder component (similar to Jira's JQL builder or existing advanced filters).
    *   The builder fetches the Metric Catalog to populate dropdowns for `metric_key`.
    *   It provides dropdowns for `operator` (>, <, =) and `threshold_type` (Absolute, Relative Delta).
    *   The form **must** call `/api/v1/rules/preview` before save (same dry-run contract as Copilot) so the admin sees match counts and an example cohort before the rule goes live.
    *   Submitting the form calls `POST /api/v1/rules` with the exact `RuleAST` JSON, bypassing the LLM entirely.

---

## 4. Foundations & Dependencies

Not all of these are blockers for the first ship. Some are true prerequisites (no rule can compile without them); others are infrastructure tracks that run **in parallel** with feature work and gate specific later phases. The phasing plan in §14 maps each item to the phase that needs it.

The largest and most failure-prone foundation — identity resolution and cohort definition — is specified in a companion document because it merits a dedicated read. This main PRD covers the analytics layer that builds on top of it.

### 4.1 Identity Resolution & Cohort Definition (companion document)

See [`IDENTITY_AND_COHORTS.md`](./IDENTITY_AND_COHORTS.md). It owns:

*   Ground-truth audit of identity-related code today (BambooHR ingestion, `insight.people`, `silver.class_people`, namespace gaps).
*   Person→Team mapping contract (gates F0).
*   Author ↔ reviewer namespace bridge (gates review-side metrics; tracked as I-D).
*   Cohort dimensions / slot contract (`team`, `function` (set, tenant-declared), `product`, plus per-function activity signals — see Identity §3.1.1).
*   Per-tenant slot mapping (`org_slot_mapping`), self-install wizard, binding composition, mapping-change semantics.
*   Three-tier role data strategy: HR-provided (primary), heuristic fallback, conditional formal `role_catalog`.
*   Cohort eligibility predicates (replaces auto-injection of any single-function HR Bool — `is_coder` was the early example; generalised to `function_eligible(F)` over an elastic vocabulary).
*   Team-level mode (first-class for tenants without rich HR).
*   Identity-specific risks and open questions.

The remainder of this PRD assumes that contract. Cross-references appear as "Identity doc §X.Y".

### 4.2 Data Integrity & "Honest Degradation" (true Phase 0 — gates F0)
*   **What**: Tracking ingestion health to prevent false verdicts.
*   **Why**: "If one number is clearly wrong, trust is lost."
*   **How**:
    *   **Backend (`analytics-api`)**: Implement an `IntegrityCheckService` that queries Airbyte/dbt sync logs.
    *   Define `DataHealthState { Ok, Partial, Unavailable }`.
    *   If `Zoom` is `Unavailable`, the Verdict endpoint must strip out Zoom-dependent rules and return `status: "Partial"` together with the list of skipped `rule_id`s and the reason, so the FE can show *what is missing* rather than silently hiding flags.
    *   **Honest-NULL principle** (consistent with the existing gold-view contract — see `20260422100000_ic-kpis-honest-nulls.sql`): when a metric value is `NULL`, the rule is evaluated as `not_applicable`, **not** `not_matched`. Zero is a real measurement; missing is missing. Rules that depend on `not_applicable` inputs do not contribute to the verdict and are reported in the `partial_reasons` array.

### 4.3 Tenant Isolation (principle, not a separate track)
*   **What**: Multi-tenant filtering across all rule and baseline queries.
*   **Why**: `analytics-api/src/api/handlers.rs:287` currently runs in single-tenant MVP mode (`MVP: single tenant — skip tenant isolation filter`). **Audit caveat (Identity §0):** the skip is broader than that single comment suggests — `query_metric()` (~line 246 same file) does not inject `tenant_id` into the assembled SQL even though it filters the metric entity by tenant. So tenant-scoping today is partial-but-not-uniform. Diagnosis layer cannot ship to multi-tenant prod without coordinated retrofit, because rule scoping (`scope=Tenant` / `scope=Team`) and role-baseline percentiles are inherently cross-employee aggregates that leak across tenants if joined without `tenant_id`.
*   **How (no separate retrofit track)**: each foundation builds in tenant isolation as it's implemented, instead of a global retrofit done after the fact:
    *   Metric-catalog implementation includes `tenant_id` plumbing from day 1 (the catalog PRD already specifies `tenant_id IS NULL` as a CHECK in v1, with the column ready to flip when tenant-custom metrics land).
    *   Org-chart implementation includes `tenant_id` on `org_units` and `person_assignments` from day 1.
    *   Connector custom-field expansion (I-E.1) emits per-tenant rows from day 1.
    *   Diagnosis-layer rule engine: tenant filtering is a query-builder primitive — the engine refuses to compile a query that does not bind a `tenant_id`. LLM prompts include only the caller's tenant's Metric Catalog rows.
    *   The legacy `analytics-api/handlers.rs:287` no-op **and the parallel skip in `query_metric()`** both get retired as part of normal handler refactors, not as a dedicated track. F0 cannot ship without at minimum auditing all `analytics-api` handlers for missing `tenant_id` injection — the "review checklist" must run at least once across the existing surface area, not only on new PRs.
*   **Why this is the right shape now**: F4a was already gated on org-chart and custom-field implementations. Doing tenant-aware schema and queries from the start of those implementations is cheaper than retrofitting after. There is no value in a separate "I-A track" — multi-tenant correctness becomes a review-checklist item on every foundation PR.

### 4.4 Time-Bucketing & Timezone Handling (constraint, not a phase — applies from F3 onward)
*   **What**: Period boundaries that match the viewer's local week, not server UTC.
*   **Why**: Every gold view today buckets by `toDate(timestamp)` in UTC (see CLAUDE.md "All dashboard dates are bucketed by UTC midnight"). For relative rules like "chat activity dropped 30% MoM", events near local midnight cross the wrong bucket and produce false positives at scale for non-UTC teams.
*   **How (interim, until per-person timezone lands in bronze)**:
    *   The rule engine restricts `RelativeDelta` and `Percentile` thresholds to **period aggregates of ≥7 days** (week-over-week minimum). One-day deltas are rejected at AST validation time.
    *   Verdict response carries a `time_basis: "UTC"` field so the FE can show the "UTC" pill consistently with `PeriodSelectorBar.tsx`.
*   **How (target)**:
    *   Track the dependency on the timezone roadmap. Original plan was `BambooHR location → Slack tz → M365 mailboxSettings.timeZone → insight.people.timezone`; the **Slack `tz` step is unavailable** — `tz` lives on `bronze_slack.users` which Slack admin API does not populate (only `users_details` daily counters are returned, with no `tz` column). The realistic path is therefore `BambooHR location` (where present) → `M365 mailboxSettings.timeZone` (primary fallback) → `insight.people.timezone`. When `people.timezone` is populated, gold views switch to `toDate(ts, p.timezone)` and the 7-day floor is lifted.

---

## 5. UI Integration & Drill-Down State

*   **What**: The `VerdictBanner` component and its interaction with existing dashboards.
*   **Why**: The diagnosis is a starting point. Users must be able to drill down into the raw data to trust the verdict.
*   **How**:
    *   **Frontend (`insight-front`)**: 
        *   Create the `VerdictBanner` React component.
        *   Bind flag clicks to Redux `slices` (or Context). Dispatching a flag click must update the global dashboard filters (e.g., `setUsersFilter([user_id_1, user_id_2])`, `setDateRange(rule_lookback_period)`).
        *   Implement `POST /api/v1/rules/{id}/feedback` for the "False Positive" / "Dismiss" buttons to track rule quality.

---

## 5.1 From Diagnosis to Action

A coloured circle and a number is not a product. The diagnosis layer's value depends on whether a manager, after seeing a flag, knows *what to do next* — otherwise we've built a prettier dashboard, not an active diagnostic tool. This section specifies the bridge from "what's wrong" to "what to do".

### 5.1.1 Action template per rule

Every rule carries a structured **action template** alongside its threshold and predicate. Authored once with the rule (seeded, admin-built, or Copilot-generated), it travels with the flag in `/verdict` responses and notification payloads.

```rust
pub struct ActionTemplate {
    pub headline: String,                     // e.g. "PR cycle time is elevated for this team"
    pub likely_causes: Vec<String>,           // up to 3 short bullets
    pub suggested_actions: Vec<SuggestedAction>,  // ordered, low → higher commitment
    pub diagnostic_links: Vec<DiagnosticLink>,    // deep-links to existing widgets/people pages
}

pub enum SuggestedAction {
    AskQuestion { prompt: String },           // "In your next 1:1, ask whether reviews are blocked on a single reviewer"
    InspectData { widget_id: String, filter: FilterRef },  // "Open the PR-cycle widget filtered to last 14 days"
    ScheduleConversation { with_role: Role }, // "Talk to your tech lead about review distribution"
    AdjustProcess { proposal: String },       // "Consider adding a second required reviewer per PR"
    Snooze { reason_required: bool },         // already exists (§8.3)
}

pub struct DiagnosticLink {
    pub label: String,                        // "PR cycle time, last 14 days"
    pub destination: LinkDestination,         // existing widget, person page, or external (e.g. Jira filter)
}
```

### 5.1.2 Hard rules

*   **Every active rule must have an action template before promotion to `active`.** Admin Rule Builder and Copilot both block save when the template is empty. Seeded rules ship with templates from migration.
*   **No prescriptive HR language.** Templates suggest investigative steps, not personnel actions. "Talk to X about Y" is allowed; "consider replacing X" is not. Copy review by the PM before a rule promotes.
*   **The action ladder is ordered low → high commitment.** First suggestion is always something the manager can do in the next 5 minutes (open a widget, check a person's PTO calendar). Higher-commitment actions (process changes, staffing) come after — a manager who only does the first action still got value.
*   **Templates are bound to predicates, not metrics.** A `mismatch_into(engineering)` flag has the same action shape regardless of which specific activity signal triggered it ("PM doing IC work — likely scope/staffing question"). This keeps the template library small and prevents per-metric proliferation.

### 5.1.3 Feedback loop on actions, not just on flags

§8.3 already has rule-level feedback (False Positive, Dismiss, Snooze). The action ladder adds:

*   **Action acknowledgement** — when a manager clicks a `SuggestedAction`, we log it (no result tracking, just intent). Surfaces in §13.3 as time-to-first-action per Red verdict.
*   **Outcome marker** (optional, manager-driven) — after 30 days, ask "did this flag turn out to be a real signal?" One-click Yes/No/Don't-know. Feeds rule quality (§12) without requiring manager-side reporting.

### 5.1.4 Honest scope of "actionability"

This is not a recommendation engine. It does not learn what works. It does not personalise. It is a **structured authoring contract** that says: "if you write a rule, you owe the consumer a sentence about what to do with it." The rule author (Constructor for seeded rules; tenant admin for Tenant rules; team lead for Team rules) is responsible for templating; we just refuse to ship rules without one.

The reason this is in the engineering PRD and not "a future product feature" is the cost of retrofitting: if rules ship without `ActionTemplate`, every existing rule needs a backfill PR later. Cheaper to make it mandatory from F1.

### 5.1.5 Scope by phase

*   **F0a** — single probe rule, action template stubbed (one headline, one suggested action). Validates the API shape lands end-to-end.
*   **F1** — full schema mandatory; 5 seeded rules each with 2–3 suggested actions and 1–2 diagnostic links into existing widgets.
*   **F2** — "what changed since last visit" surface uses action templates to summarise: not just "Red on PR cycle time" but "Red on PR cycle time, suggested first step: open the widget filtered to last 14 days".
*   **F4a+** — Admin Rule Builder enforces template author flow; Copilot generates a template draft along with the AST and shows it in dry-run preview before save.
*   **F7** — Slack/email digests render the top suggested action inline, not just the verdict colour. (Without action context, push notifications become noise; with it, they become tasks.)

---

## 6. Security, Compliance & Tenant Isolation

*   **What**: Ensuring the system complies with enterprise security and data privacy laws (GDPR Article 22).
*   **Why**: AI features processing employee performance data represent high compliance risks.
*   **How**:
    *   **Tenant Isolation**: The `RuleAST` and `role_profiles` tables must have strict `tenant_id` foreign keys. LLM prompts must never mix data from multiple tenants.
    *   **GDPR Compliance**: The system is strictly *recommendational*. We must explicitly log the exact mathematical reason (the specific metrics and threshold) for every flag generated. The UI must always provide a "Show Details" link to the raw data.
    *   **Feature Toggles**: The `analytics-api` must support a tenant-level configuration flag `disable_llm_features`. If `true`, the Chat-Copilot and Auto-Profiles are disabled, and the Tenant Admin must configure everything manually via CRUD APIs. When the flag flips to `true` mid-life, existing LLM-generated `role_profiles` remain active (already approved by a human) but no new generations occur until the flag is unset.

---

## 7. Verdict Aggregation Algorithm

The single most product-defining piece of logic, and the one most likely to be argued about. Specified explicitly so it does not get re-invented per consumer.

### 7.1 Per-rule outcome
Each rule evaluation returns one of:
*   `matched(severity)` — `severity ∈ {Yellow, Red}` (configured per rule)
*   `not_matched`
*   `not_applicable` — input was honest-NULL, cohort below `min_cohort_size`, or dependency `Unavailable`
*   `error` — engine-level failure (logged; treated as `not_applicable` for the verdict)

### 7.2 Verdict roll-up (per cohort)
The cohort verdict is computed as **worst-of**, with a weight floor to avoid a single noisy rule dominating:
*   Any `Red` from a rule with `weight ≥ 1.0` → `Red`
*   ≥2 distinct `Yellow` matches **or** any `Red` from a `weight < 1.0` rule → `Yellow`
*   Otherwise → `Green`
*   If `>50%` of relevant rules returned `not_applicable` → `Partial` (renders as Green with a "limited data" badge; never Red on partial data).

`weight` is a rule-level field (`f32`, default `1.0`) so Tenant Admins can de-emphasize noisy seeded rules without disabling them.

### 7.3 Conflict between Tenant and Team rules
Both contribute to the same roll-up. If a `Tenant`-scoped rule and a `Team`-scoped rule fire on the same `metric_key` for the same person, both flags are shown (de-duplicated by `(metric_key, person_id)` for display, but counted once). Tenant rules cannot be overridden by Team rules — only suppressed via Snooze (§8.3).

### 7.4 Locks in the Metric Catalog are authoritative

The Metric Catalog supports `is_locked` rows that act as resolution ceilings (see §2.1). Because absolute thresholds live exclusively in the catalog (§3.2 — no `Literal` variant), locks are unambiguously authoritative:

*   A rule with `ThresholdSource::Catalog { kind }` resolves through the catalog's normal chain. A lock at any scope stops resolution; the rule fires against the locked value.
*   No way for a rule to "bypass" a lock — that would require a parallel threshold storage, which we deliberately don't have.
*   Verdict's `explanation` includes `resolved_from` and `is_locked` so admins can trace the threshold to its policy source.

---

## 8. Rule Lifecycle

### 8.1 States
`draft → preview → shadow → active → deprecated`. Only `active` rules contribute to verdicts. `shadow` rules evaluate on every dashboard load and log outcomes but are invisible to the consumer — used for backtest before promotion.

### 8.2 Backtest / Shadow mode
Before promotion to `active`, every new rule (Tenant or Team scope) must pass a backtest over the prior 90 days:
*   Match-rate per week (sanity-check: ≥1 match in 4 of 13 weeks; ≤80% of cohort flagged in any week — both bounds are warnings, not blockers, but require an explicit acknowledgement).
*   Stability of cohort (rule should not flip 100% of members in/out across consecutive weeks — likely a bad threshold).

Backtest results are stored on the rule and surfaced in the Admin UI before activation.

### 8.3 Snooze & Acknowledge
*   `POST /api/v1/flags/{flag_id}/snooze` — silences a flag for `(team_id, rule_id, person_id?)` for `N` days. Audit-logged.
*   Snoozes are bounded (max 30 days) and require a free-text reason.
*   `POST /api/v1/rules/{id}/feedback` (already in §5) feeds rule-quality metrics; a rule with `false_positive_rate > 30%` over 30 days raises a maintenance alert (see §13).

### 8.4 Versioning
Every save creates a new immutable version. Verdict history (§9.2) references `(rule_id, version)` so historical Red→Green transitions remain interpretable after threshold edits.

---

## 9. Cohort & Baseline Edge Cases

### 9.1 Minimum cohort size
*   `min_cohort_size` (tenant-level, default `5`) — when an evaluation cohort is smaller, the rule returns `not_applicable`. Protects both statistical validity and individual privacy (no "this single person is at P10" exposures).
*   For percentile rules, the engine refuses to compile if `threshold_value` × cohort < 1 (e.g. P5 over a cohort of 6 — meaningless).

### 9.2 Cold-start
*   New tenant with <4 weeks of history: relative rules are auto-disabled; absolute rules from the global Base library remain.
*   `role_profiles` cannot be approved on <4 weeks of fingerprint data; the queue UI shows "insufficient data" instead of an LLM draft.
*   A `verdict_history` table records `(tenant_id, cohort_id, ts, verdict, contributing_rule_versions[])` so trend analysis (Red→Yellow over weeks) is possible — a hard requirement for proving ROI.

### 9.3 Baseline drift & freshness
*   Role fingerprints are recomputed nightly (§3.4). When the new fingerprint diverges from the active profile by >25% on any P50 metric, the profile flips to `status = drift_review` and a notification is posted to the Tenant Admin queue.
*   Profiles older than 180 days without re-approval are auto-flagged `stale` and rules referencing them log a warning but continue to evaluate (do not silently disable).

### 9.4 Role transitions
*   When a person's slot value changes mid-period (promotion, team move), the person is included in the **prior** cohort for periods ending before the change date and the **new** cohort after, using `person_assignments` temporal history from the org-chart domain (Identity doc §3.2.5; org-chart PRD).
*   Verdicts emitted during a transition surface a flag `"role_changed_in_period"` for transparency.

### 9.5 Anti-gaming notes (Goodhart)
This is a measurement system applied to humans; any metric that becomes a target degrades. Mitigations baked in:
*   No single metric drives a Red verdict alone unless `weight ≥ 1.0` and explicitly approved at Tenant scope.
*   Counter-pairs: a rule "low commits" must be co-evaluated with "low PR review activity" — the engine supports `composite_rule` (AST union) so optimizing one metric in isolation does not clear the flag.
*   Verdicts are surfaced to the manager only, never to the IC, and never as a numeric score. UI copy must avoid "performance" framing — flags describe *signals to discuss*, not *deficiencies*.

---

## 10. Performance, Latency, and API Contracts

### 10.1 Latency budget
*   `/verdict` endpoint p95 ≤ 800 ms for a team of ≤50, ≤2 s for a tenant-wide aggregate of ≤1000.
*   Rule evaluation is cached per `(rule_version, period, cohort_hash)` in Redis with a TTL of 1 hour. Fingerprint baselines are cached daily.
*   The query builder (§3.1) batches all rules touching the same `*_bullet_rows` view into a single ClickHouse query per period.

### 10.2 `/verdict` response shape
```json
{
  "cohort_id": "team:123",
  "verdict": "Yellow",
  "time_basis": "UTC",
  "data_health": "Partial",
  "partial_reasons": [
    {"source": "zoom", "state": "Unavailable", "skipped_rule_ids": ["..."]}
  ],
  "flags": [
    {
      "rule_id": "...", "rule_version": 3, "severity": "Yellow",
      "metric_key": "pr_cycle_time_h", "matched_cohort": ["person_id_1", ...],
      "explanation": {
        "metric_value": 72.5,
        "threshold": {
          "source": "catalog",            // "catalog" | "percentile" | "relative_delta"
          "value": 48.0,                  // resolved threshold the rule fired against
          "kind": "warn",                 // catalog kind (only when source=catalog)
          "resolved_from": "team+role",   // catalog scope that supplied it (only when source=catalog)
          "is_locked": false              // true if catalog row was locked
        },
        "baseline_value": 48.0,
        "lookback_period": {"from": "2026-04-01", "to": "2026-04-29"},
        "predicate": "engineering_eligible"   // which eligibility predicate matched the cohort (Identity doc §4.3)
      }
    }
  ],
  "generated_at": "2026-04-29T10:00:00Z"
}
```
*   `explanation` is **mandatory** on every flag — directly enables GDPR Art. 22 "show details" and the FE drill-down.
*   `Cache-Control: private, max-age=300` for normal responses; `no-store` when `data_health != Ok`.

### 10.3 Versioning
*   All endpoints under `/api/v1/`. Breaking schema changes require `/api/v2/` and a 90-day overlap. The `verdict` shape carries a `schema_version` field so FE can degrade gracefully on minor additions.

---

## 11. Distribution: Notifications & Digests
The PRD's "active diagnostic tool" framing requires push, not pull.

*   **In-product**: existing `VerdictBanner`, plus a new "What changed since last visit" surface diffing the current verdict against the last seen verdict per user.
*   **Email digest** (opt-in, weekly default): summary of Red/Yellow flags for teams the user manages.
*   **Slack/M365** (Phase 2): direct-message notification on Red transitions, throttled to 1/day per cohort.
*   **Critical (Red)**: never auto-pages. This is a managerial tool, not an oncall system.
*   All channels honour the same `disable_llm_features` and per-user notification preferences. Notification payloads include the same `explanation` object as the API.

---

## 12. Observability of the Engine Itself

Diagnosis Layer is itself a piece of infrastructure that needs monitoring:

*   **Per-rule metrics** (Prometheus): `rule_eval_duration_seconds{rule_id}`, `rule_match_rate{rule_id, scope}`, `rule_false_positive_rate_30d{rule_id}`, `rule_not_applicable_rate{rule_id, reason}`.
*   **LLM telemetry**: `copilot_tokens_total{tenant_id}`, `copilot_validation_failures_total{reason}`, `copilot_retries_total`, golden-set pass rate per deploy.
*   **Engine alerts**:
    *   Rule firing on >80% of cohort for 7 consecutive days → likely misconfigured, page the Tenant Admin queue.
    *   Rule firing 0 times in 30 days → "dead rule" suggestion.
    *   `false_positive_rate_30d > 30%` → maintenance review queue.
*   **End-user telemetry** (FE): `verdict_banner_view`, `flag_clicked`, `flag_drilldown_completed`, `flag_dismissed_false_positive`. Used to compute the success metrics in §14.

---

## 13. Success Metrics (How We Know This Worked)

### 13.1 Adoption
*   ≥60% of managers in pilot tenants open the dashboard ≥3×/week within 60 days of GA (vs. current baseline of ~once/week).
*   ≥40% of managers click through ≥1 flag per session.

### 13.2 Quality
*   Aggregate `false_positive_rate_30d` across active rules ≤ 20%.
*   ≥70% of generated `RuleAST` from Copilot are saved without manual editing.
*   Golden-set NL→AST pass rate ≥ 90%; no regression on model bumps.

### 13.3 Outcome (leading indicator)
*   Time-to-insight: median time from "team has Red verdict" to "manager opens drill-down" ≤ 24h.
*   Verdict trajectory: ≥30% of `Red` verdicts transition to `Green/Yellow` within 30 days (proxy for "managers act on flags").

### 13.4 Trust
*   ≤5% of flags marked "False Positive" by managers per month (per-rule cap is the trigger; aggregate is the dashboard).
*   Zero P0 incidents of verdicts derived from honest-NULL data being shown as `not_matched`.

### 13.5 Health of the engine itself

The above metrics measure *the product*. We also need a metric for *whether the engine is doing anything* — because with current dependency gaps (review-side aliases not emitted, HR custom fields flat, person-domain override = dbt PR), the most likely silent failure mode is "everything is `not_applicable` and the banner is permanent Partial."

*   **Applicable rule rate** ≥ 70% across active rules per active tenant. If a tenant routinely sees <70% of relevant rules return a real outcome (not `not_applicable`), the diagnosis layer is degenerate for that tenant — the verdict carries no information. Surfaced per-tenant in admin telemetry; aggregate in §13 dashboard.
*   **Per-source alias coverage** monitored as a leading indicator: e.g. "% of `silver.fct_git_review` reviewer rows resolvable to `person_id`". Sub-50% means review-side rules are decorative.
*   These are diagnostic of the *plumbing*, not of *manager behaviour* — they protect against the failure mode where the product looks clean (low FP rate, high adoption) only because no rules ever fire.

---

## 14. Phased Rollout

### 14.1 Phasing principles

*   **Each phase is shippable on its own.** If the next phase is cancelled, the previous phase still delivers value.
*   **Walking skeleton first.** The thinnest possible end-to-end vertical slice (one rule → one banner) ships before any breadth — to surface integration bugs while change is cheap.
*   **Two parallel tracks.** Feature phases (F0–F7) build the user-visible product. Infra tracks (I-C, I-D, I-E) run alongside and gate specific feature phases. Some infra tracks (I-D, I-E) sit on adjacent in-flight work owned by other PRs in the same team; this PRD declares dependencies, not deadlines, on those.
*   **Single-tenant dogfood first.** Constructor's own engineering org is Tenant 0 from F0. Multi-tenant correctness is built into every foundation as it ships (§4.3) rather than being retrofitted as a separate track.
*   **Risky work after foundation.** Anything LLM, anything relative-threshold, ships *after* the deterministic foundation has proven itself in dogfood.
*   **T-shirt sizes, not commitments.** Estimates below are a planning tool, not a contract.

#### Team and velocity assumptions (read this before believing any week count)

The whole Insight org is **1 PM + 6 developers**. There is no separate diagnosis team, identity team, connectors team, or customer success function — those are slices of attention from the same six people. When this PRD says "owned by us" vs "adjacent in-flight work", it's labelling which slice of the same team's attention picks up the work, not which team.

Two consequences for week counts:

1.  **All estimates assume Claude-Code-assisted velocity.** The whole team writes code with Claude Code; raw code generation is not the bottleneck. Design decisions, integration, and review capacity are. Treat week ranges as calendar-time *with that working condition* — not as person-weeks of traditional dev time. Numbers have already been compressed for this; don't compress them further on top.

2.  **Review capacity is the real constraint.** With 6 devs reviewing each other (and the PM as approver on PRDs), every parallel infra track adds review load. If multiple I-tracks run concurrently, calendar time grows because review queue lengthens, not because typing takes longer. Sequencing I-tracks rather than maxing parallelism may ship faster.

PRD-language like "team owns X" is shorthand for "this PR or set of PRs is the place this work shows up" — if you find yourself reading it as separate org units, re-read with this team size in mind.

### 14.2 Track overview

```text
Feature track:    F0 → F1 → F2 → F3 → F4a → F5 → F6 → F7
                  └─────dogfood──┘     └────design partners────┘   └──GA──┘

Our own work (the diagnosis-layer slice of dev attention):
  I-C (LLM Eval Harness, 2-3w):     ═══ gates F5, F6

Tenant isolation (§4.3): not a track — a review checklist on every foundation PR.

Adjacent in-flight work in the same team (declared dependencies):
  I-D (Author/reviewer bridge):    ─ ─ ─ identity-resolution roadmap; unblocks review-side rules
  I-E (HR ingestion expansion):    ─ ─ ─ connector / ingestion work; improves F4a confidence, lifts §4.4 floor
  Metric-catalog impl:             ─ ─ ─ in-flight; F0 ships minimum-viable slice on top
  Org-chart impl:                  ─ ─ ─ not started; gates F4a
  Connector custom-fields plumbing:─ ─ ─ not started; gates F4a (I-E.1 detail)
```

### 14.3 Feature phases

#### F0 — Walking Skeleton (3–5 weeks; revised from 2–3w after code audit)

*   **Goal**: Prove the pipeline end-to-end with **one probe rule** on Constructor's own data, and use it to learn which metrics produce stable, low-noise signal in the wild.
*   **Reality check (Identity §0 audit, May 2026)**: metric-catalog has **no code** — neither `metric_catalog`/`metric_threshold` tables, nor a `GET /catalog/metrics` endpoint, nor any open PR scheduled to deliver them. Today's de-facto threshold storage is `insight-front/src/screensets/insight/api/thresholdConfig.ts` (FE-side). F0 therefore must choose between two shapes — **F0a (genuine walking skeleton)** ships first to unblock learning; **F0b (catalog MVP)** follows.
*   **F0a — Walking skeleton (2–3w, no catalog dependency).** One probe rule with a **literal threshold hardcoded in the engine** (yes, this temporarily violates §3.2 "no `Literal` variant" — the violation is scoped, time-boxed, and tracked in F0b for removal). Threshold value mirrors the FE `thresholdConfig.ts` entry so manager-visible numbers are consistent. Goal: validate gold→engine→FE plumbing without coupling the calendar to a foundation that has no owner.
*   **F0b — Catalog MVP (1–2w, after F0a).** Minimum viable slice of metric-catalog: `metric_catalog` + `metric_threshold` tables (per catalog PRD v1 schema), one seed migration for the probe metric's `product-default`, `GET /catalog/metrics` returning resolution chain limited to `product-default`. F0a's literal-threshold rule is rewritten to consume the endpoint; the `Literal` exception is removed. Locks, tenant/role/team scopes, admin CRUD remain out — they come with subsequent catalog work, **owned by whoever picks up the catalog implementation track** (no current owner; this is a real backlog gap, not "in flight").
*   **Probe rule selection**: chosen because it stresses the pipeline (multiple sources, NULL handling, person→team resolution), **not because it's product-important**. Concrete starter candidate: `pr_cycle_time_h > 48h` (static literal in F0a, catalog-resolved in F0b). Final pick made at F0a kickoff — anything that exercises the full path is fine.
*   **In scope (F0a + F0b)**:
    *   F0a literal-threshold rule + `VerdictBanner` rendering.
    *   F0b minimum-viable catalog tables/endpoint and rule rewrite to consume them.
    *   One rule expressed as a Rust constant (no DB-backed rule storage, no admin UI, no tenant scope, no aggregation beyond "match → Yellow").
    *   `/verdict` endpoint shape stubbed (one flag at most).
    *   **Tenant-isolation audit pass** across `analytics-api` handlers (§4.3 — must run once, including fixing `query_metric()` skip and any others discovered).
    *   **Data-correctness gate** for the probe metric: independent verification that `pr_cycle_time_h` values match a hand-computed sample on the dogfood tenant before banner activation (lesson learned: customers have caught wrong numbers in the past because we did not gate on this).
*   **Out of scope**: Catalog admin CRUD, locks, non-`product-default` scopes, all other phases.
*   **Depends on**: Identity doc §2.1 (person→team — already largely works on email-keyed sources), §4.2 (honest-NULL contract — partly there via existing migrations). Author-side metric only — F0 does NOT depend on review-side alias work.
*   **Exit criteria**: F0a: probe rule fires on Insight team's own dashboard with real production data; data-correctness check passes; tenant-isolation audit complete with all skips fixed; `not_applicable` propagates correctly when the underlying source is `Unavailable`. F0b: catalog tables seeded; `Literal` variant removed from engine. **F0 has run for ≥2 weeks on real data after F0b** so F1 rule selection can be informed by observed signal stability.
*   **Why this matters**: Cheap forcing function for "does our gold→engine→FE plumbing actually compose?" Catches honest-NULL handling bugs and aggregation assumptions before they're baked in. Splitting F0a/F0b is what makes "walking skeleton" actually thin — not bundled with a foundation that has no owner.

#### F1 — Deterministic MVP (4–6 weeks)

*   **Goal**: A managed library of absolute-threshold rules, authored by tenant admins.
*   **F1 rule selection**: the 5 seeded rules are **chosen at F1 kickoff, after ≥2 weeks of F0 running on real data** (deliberately deferred from PRD-time choice — see F0 above). Selection criteria: (a) signal stability observed during F0 — metric isn't wildly noisy week-over-week, (b) catalog has a `product-default` threshold, (c) the metric resolves cleanly via author-namespace (not blocked on I-D), (d) covers diverse failure modes so worst-of aggregation isn't dominated by one signal type. PM decides; no separate workshop (the team is 1+6 — see §14.1 team note).
*   **In scope**:
    *   `RuleAST` schema + storage (with `tenant_id` column and query-builder enforcement from day 1, per §4.3).
    *   5 seeded `Global` rules, all `ThresholdSource::Catalog`.
    *   Admin Rule Builder UI (§3.5) with mandatory dry-run preview.
    *   `/verdict` endpoint with worst-of aggregation (§7).
    *   `VerdictBanner` + drill-down (filter dashboard by `matched_cohort`).
    *   Snooze + Feedback APIs (§8.3).
    *   `DataHealthState` plumbed end-to-end with `partial_reasons`.
*   **Out of scope**: Relative rules, percentile rules, role profiles, LLM, notifications, verdict history, multi-tenant safety, backtest UI.
*   **Depends on**: F0 (≥2 weeks of data), Identity doc §2.1, §4.2, metric-catalog seeded for the 5 chosen metrics at `product-default` scope.
*   **Exit criteria**: Constructor tenant uses it ≥1×/week by ≥3 EMs; FP rate ≤30% on seeded rules; zero P0 honest-NULL incidents.
*   **Risk**: Worst-of aggregation feels noisy with too many rules → mitigation: cap at 5 seeded rules, require explicit acknowledgement to add a 6th.

#### F2 — Verdict History & "What Changed" (2–3 weeks)

*   **Goal**: Trends, not just snapshots — needed to prove ROI and to compute "what changed since last visit".
*   **In scope**: `verdict_history` table (`tenant_id`, `cohort_id`, `ts`, `verdict`, `contributing_rule_versions[]`); 4-week trend chart; "what changed" diff surface; rule version pinning so historical verdicts remain interpretable after threshold edits.
*   **Out of scope**: Cross-tenant analytics, predictive trends, exports.
*   **Depends on**: F1.
*   **Exit criteria**: Per-team verdict trajectory visible for last 30 days; ≥30% of `Red` verdicts in dogfood transition to `Green/Yellow` within 30 days (§13.3 outcome metric is now measurable).

#### F3 — Relative Rules (single-tenant, no roles) (3–4 weeks)

*   **Goal**: Week-over-week and month-over-month deltas without needing role data.
*   **In scope**:
    *   `ThresholdSource::RelativeDelta` extension to AST (matches §3.2 enum).
    *   `scope=Team` percentile-within-team rules (cohort = members of one team — no cross-team statistics yet).
    *   Backtest/shadow mode (§8.2) — mandatory before activation.
    *   7-day-period floor enforced at AST validation (per §4.4 UTC-bucketing constraint).
*   **Out of scope**: Cross-team / cross-role percentiles (need org-chart in flight + multi-tenant correctness via §4.3); composite/counter-pair rules.
*   **Depends on**: F1, F2 (history needed for deltas), §4.4 acknowledgement.
*   **Exit criteria**: ≥3 relative rules active in Constructor tenant; backtest UI shows match-rate sanity for each; no false-positive incidents from timezone-boundary events.
*   **Risk**: UTC bucketing causes false positives near local midnight → mitigation: 7-day floor + dogfood-only until we measure FP rate in practice.

#### F4a — HR-provided Cohort Dimensions (7–9 weeks of F4a logic + 2–4 weeks of org-chart implementation prerequisite; first multi-tenant ship)

*   **Goal**: Cohort-based rules across declared functions (`team`, `function` set, `product`, plus per-function activity signals — Identity §3.1.1) on real HR-provided dimensions, with heuristic fallback for sparse-HR tenants. Self-installable by Tenant Admin without Constructor onboarding involvement.
*   **Hidden prerequisite — org-chart hasn't been implemented yet** (only the PRD has merged; implementation not started at writing). `person_assignments`, `org_units`, the v1 assignment_type enum — none exist as code today. F4a relies on these for temporal slot historization. Either (a) F4a absorbs minimum-viable org-chart implementation as in-scope work (table + assignment_types + bulk-load + point-in-time query), or (b) we wait for someone to ship org-chart separately. Either way the calendar time is real — not absorbed into the 7–9w F4a estimate. **F4a cannot be scheduled until org-chart implementation is in flight.**
*   **In scope**:
    *   **Either bring up org-chart `person_assignments` minimally, or coordinate with whoever picks up org-chart implementation.** No further F4a work proceeds until `person_assignments` accepts writes.
    *   Ingestion expansion via I-E to pull arbitrary BambooHR custom fields into bronze (this is also more than a flag — see I-E.1 below).
    *   Add new `assignment_type` values (`product`, `subteam`, plus a generic `function_signal_binding` carrier per Identity §3.1.1) to org-chart's enum.
    *   Gold layer: `insight.people_org` view + `*_bullet_rows` enrichment with org slots.
    *   AST `cohort_filter` accepts any populated slot. **No auto-injection** — each rule declares its cohort explicitly via named eligibility predicates (Identity doc §4.3).
    *   Eligibility-predicate library (~6–8 named predicates: `shipped_code_90d`, `reviewed_code_90d`, `engineering_eligible`, `active_recently`, `manages_team`, `tenured_30d`).
    *   `org_slot_mapping` table + resolver (Identity doc §3.2) supporting priority (`First`) and AND (`All`) composition.
    *   `org_slot_mapping_history` + verdict trend break-point UI for mapping changes.
    *   Heuristic fallback (Identity doc §4.2) per-slot for tenants that don't expose a given slot.
    *   Manual overrides via the person-domain canonical flow (`*_source = 'manual'` per person PRD §5.2) — admin override UI calls into that mechanism rather than maintaining a parallel table.
    *   **Self-install wizard MVP** (BambooHR fields only): discovered-fields → suggested bindings → coverage check → "what works" preview. Tenant-Admin-only. Slack and heuristic-on-`job_title` bindings via JSON in F4a; UI in follow-up.
    *   **No-fields-match flow**: wizard explicitly drops the tenant to F1/F2/F3 mode (Identity doc §4.4) without enabling F4a features. Not a degraded fallback — a supported mode.
    *   Drill-down behaviour driven by rule predicate (Identity doc §4.3), with "Show all (incl. non-eligible)" toggle. Self-view always allowed.
    *   Mapping coverage feedback nightly job (≥5 people **and** ≥5% threshold for surfacing disagreements).
    *   Manual role profiles — admin-curated baselines per cohort, **no LLM yet**.
    *   `min_cohort_size` enforcement (§9.1).
    *   First multi-tenant deploy.
*   **Out of scope**: LLM-generated profiles; LLM-generated mapping suggestions (heuristic only in F4a); drift detection; formal `role_slug` taxonomy.
*   **Depends on**: F1, **org-chart in flight with tenant_id from day 1** (§4.3), **I-E.1 + I-E.2 complete** (BambooHR custom-field ingestion + slot mapping resolver, both tenant-aware from day 1).
*   **Exit criteria**: ≥2 design-partner tenants with HR-dimension rules active; `function_eligible(engineering)` cleanly excludes non-engineering activity in dogfood (and `mismatch_into(engineering)` fires on at least one real out-of-function signal); cross-tenant query never compiles without `tenant_id`; for dogfood tenant ≥4 slots populated with ≥80% coverage; for design-partner tenants the onboarding validation passes for ≥3 slots.
*   **Risk if HR is sparse**: Tenant has only `job_title` populated → fallback regex must do all the work → product looks coarse. Mitigation: clear UI honesty + admin override + tenant onboarding gate that warns before activation.
*   **Risk if HR is rich but ingestion is hard**: §16.11 might surface a non-trivial integration (custom Sheets/Excel sync, export pipelines). Mitigation: explicitly scope I-E only after §16.11 is answered; don't commit to F4a timeline until then.

#### F5 — LLM-generated Role Profiles (4–6 weeks)

*   **Goal**: Auto-Profiles pipeline (§3.4) — LLM drafts profile, human approves.
*   **In scope**: Nightly fingerprint job (dbt); LLM draft generation; review queue UI with diff view; drift detection (`status = drift_review`); `disable_llm_features` tenant flag; drift acceptance UX.
*   **Out of scope**: NL→Rule Copilot.
*   **Depends on**: F4a, **I-C complete** (eval harness in CI).
*   **Exit criteria**: ≥1 tenant approves an LLM-drafted profile without manual edits; drift detection raises a real change in dogfood data.
*   **Note on coarseness**: while `person.persons.role` is raw text, baselines lump together everyone with similar role strings ("Senior Backend Engineer" and "Backend Engineer" might end up in different buckets). Once identity-domain normalization lands and `role` becomes a stable slug, baselines tighten automatically with no PRD change.

#### F6 — NL→Rule Copilot (4–6 weeks)

*   **Goal**: Team Leads create `scope=Team` rules in chat (§3.3).
*   **In scope**: `/copilot/chat` with strict JSON output; AST validation + prompt-injection defense; rate limit + per-tenant token budget; bounded retry (max 2); golden-set in CI.
*   **Out of scope**: Multi-turn conversation, rule editing via chat, voice.
*   **Depends on**: F1 (deterministic foundation), I-C.
*   **Exit criteria**: ≥70% of Copilot-generated ASTs saved without edits; golden-set pass rate ≥90%; zero cross-tenant catalog leak in red-team testing.

#### F7 — Distribution: Push, not Pull (3–4 weeks)

*   **Goal**: Active diagnostic tool — verdicts find the manager, not the other way around.
*   **In scope**: Weekly email digest (opt-in default); Slack DM on Red transitions, throttled 1/day per cohort; per-user notification preferences; same `explanation` payload as `/verdict`.
*   **Out of scope**: M365 messaging, mobile push, paging behaviour for Critical (deliberately never built — §11).
*   **Depends on**: F2 (history needed for "what changed since last visit").
*   **Exit criteria**: ≥40% of managers in pilot tenants click through ≥1 flag from a digest within 60 days (§13.1).

### 14.4 Infra tracks (parallel)

> Tenant isolation is **not** a separate track here. It's a review checklist applied to every foundation PR (§4.3) — metric-catalog, org-chart, custom-field plumbing, our own rule engine all ship tenant-aware from day 1. The `analytics-api/handlers.rs:287` MVP no-op gets retired through normal handler refactors.

#### I-C — LLM Eval Harness (2–3 weeks; before F5)
*   **Scope**: ≥40 NL→AST golden examples in `analytics-api/tests/copilot_golden.json`; CI pass-rate gate; model-version + prompt-version tracking on every generated rule.
*   **Gate**: F5 and F6.

#### I-D — Author ↔ Reviewer Namespace Bridge (size unknown; not on diagnosis team's roadmap)
*   **Why parallel**: Any rule referencing review-side metrics (`pr_review_time`, `reviews_given`, `time_to_first_review`) is impossible without this. Today `silver.fct_git_review.person_key` is GitHub login / Bitbucket display_name; `silver.fct_git_pr.person_key` is `lower(author_email)`. They cannot be joined.
*   **Scope (when scoped)**: A bridge table or extension to the `identity` service that maps git-host logins to BambooHR-keyed `person_id`. Likely needs new ingestion (GitHub `/users/{login}` → email; Bitbucket workspace member API).
*   **Owner**: identity-resolution roadmap, not the diagnosis-layer slice. We declare the dependency and do not block on it — until I-D ships, AST validation rejects review-side `metric_key`s with `not_supported_yet`.
*   **Gate**: Any review-side rule (no specific phase — opens up new rule library when delivered).

#### I-E — HR Ingestion Expansion (3–5 weeks; spread across diagnosis-layer slice and connector work)
*   **Why parallel**: BambooHR connector currently pulls 7 hardcoded fields. Realistic enterprise HR data has 15+ fields including `Coder`, `Product`, `Teams`, `Subteam`, `Subdepartment`, `Department Owner`, `Function Owner` — all in BambooHR custom fields, not in the current connector pull. Other tenants will have different custom field names. Diagnosis layer needs (a) generic ingestion of arbitrary BambooHR custom fields, and (b) per-tenant slot mapping (Identity doc §3.2) to bind them to logical cohort slots.
*   **Scope**:
    *   **(I-E.1) BambooHR connector expansion** — connector work. Today the connector has a `bamboohr_employees_custom_fields` config flag declared in `connector.yaml` but no `Map(String, String)` schema in bronze and no unified mechanism that propagates custom-field values through silver→gold. I-E.1 covers: bronze schema change to store custom fields as a map column, dbt unwrap into typed silver columns, `/discovered-fields` endpoint for the slot-mapping UI. Heavier than "flip the flag".
    *   **(I-E.2) Slot mapping config & resolution** — diagnosis-layer slice. `org_slot_mapping` table in MariaDB; resolver feeds the org-chart pipeline that writes `person_assignments`; mapping CRUD API. Coordinated with the org-chart PR for any new `assignment_type` values.
    *   **(I-E.3) ~~Slack `users.tz` ingestion~~ — dropped.** Slack admin API does not return user roster (`bronze_slack.users` is empty); the `tz` field cannot be populated from current Slack access. Closest realistic alternative: pursue Slack Enterprise Grid SCIM/Web API access for user profiles (separate procurement decision, out of scope for this PRD).
    *   **(I-E.4) M365 Graph `mailboxSettings.timeZone`** — promoted from "optional fallback if Slack tz isn't populated" to **primary path for per-person timezone**, since the Slack route is not feasible (see I-E.3 above).
*   **Owner**: connector work for I-E.1 / I-E.3 / I-E.4; diagnosis-layer slice for I-E.2.
*   **Gate**:
    *   F4a hard-gates on I-E.1 + I-E.2 (no point in F4a without ingestion + mapping).
    *   Lifting the §4.4 7-day floor is gated on I-E.4 reaching ≥80% of active employees with non-NULL timezone (I-E.3 dropped — Slack admin API does not deliver `tz`).

### 14.5 Dependency summary

| Phase | Hard depends on | Soft depends on (better-with) |
|---|---|---|
| F0a (skeleton, literal threshold) | Identity doc §2.1, §4.2; tenant-isolation audit complete | — |
| F0b (catalog MVP) | F0a; ownership escalation for catalog work resolved | — |
| F1 | F0a + F0b | — |
| F2 | F1 | — |
| F3 | F1, F2, §4.4 (constraint) | — |
| F4a | F1, **org-chart in flight (tenant-aware, §4.3)** | F2 (better trend story); **I-E** (per-tenant `jobTitle` validation) |
| F5 | F4a, **I-C** | — |
| F6 | F1, **I-C** | F5 (shared eval infra) |
| F7 | F2 | F4a (better with cross-team digests) |
| Review-side rules in any phase | **I-D** | — |

### 14.6 If we have to cut

If the budget shrinks mid-flight, the **minimum viable shippable product** is **F0a + F0b + F1**. That alone moves Insight from "passive dashboard" to "managers see one persistent verdict" — measurable, defensible, and worth shipping even if F2+ never lands. F0a alone (skeleton with literal threshold) is **demoable but not shippable** — the `Literal` exception must be removed before tenant exposure.

Cut order under pressure:
1.  Cut F7 (distribution) — banner-only is still useful.
3.  Cut F6 (Copilot) — Admin Rule Builder covers the authoring need.
4.  Cut F5 (LLM profiles) — manual profiles from F4a still work.
5.  Cut F4a (inferred-role cohorts) — ship F1+F2+F3 with team-relative rules only. Honest framing: "we diagnose at the team level; role-level requires HR data we don't have."
6.  Cut F3 (relative rules) — ship F1 with absolute thresholds only.
7.  Never cut F0, F1, F2 — they are the product.

### 14.7 Dogfooding gate

Constructor's own engineering org is Tenant 0 from F0. The Insight team's PM is the Tenant Admin. This is non-negotiable: we ship F1 to ourselves, run for **at least 4 weeks** before opening F4 to a design partner, and any phase that fails on our own data does not promote to design partners. We eat our own dog food before any external team sees a Red verdict on real employees.

**Cohort-size note.** "Constructor as Tenant 0" means the **whole Constructor organisation** — hundreds of people across multiple engineering orgs, not the 7-person Insight team alone. Cohort-statistics requirements (`min_cohort_size = 5`, percentile rules ≥20 cohort members) are easily met against the broader employee base. Mistaking Tenant 0 for "the Insight team itself" would invalidate dogfood as a statistical exercise; it does not, because Tenant 0 is org-level. The Insight team is just the **first set of managers** to use the product on real Constructor employee data — they are the consumers of dogfood, not its cohort.

### 14.8 What we deliberately are not building

Surfaced explicitly so it doesn't sneak back in:

*   Paging on Critical verdicts (§11) — this is a managerial tool, not oncall.
*   IC-visible flags about themselves — manager-only until explicit user research says otherwise (Open Question §16.3).
*   Cross-tenant baselines / "industry benchmarks" — compliance minefield, deferred indefinitely (§16.2).
*   A rule-template marketplace — defer past F7 (§16.4).
*   Numeric "performance scores" — verdicts are categorical Green/Yellow/Red; no aggregate score on a person.
*   A role taxonomy / `role_alias` normalization layer of our own — that's identity-domain work, not ours. We consume `person.persons.role` as a string. When normalization lands in identity-domain, we benefit transparently.
*   Building HR connectors beyond what BambooHR currently exposes — that's connector-work scope (I-E). We declare the dependency, we don't pre-empt.

---

## 15. Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| A foundation PR (catalog, org-chart, connector custom-fields, our rule engine) ships without tenant isolation and we don't catch it in review | Medium | Critical | Tenant isolation is a review checklist (§4.3). Diagnosis-layer query builder refuses to compile a query without `tenant_id`. F0–F3 ship to single dogfood tenant which masks regressions — keep the checklist hard even when there's only one tenant. |
| Identity & cohort risks (mapping, predicates, role data, namespace bridge) | — | — | See Identity doc §5. |
| Goodhart's-law erosion: managers gaming flags | Medium | High | Counter-pair rules (§9.5); manager-only visibility; `weight` floor on Red. |
| F1 ships before metric-catalog has thresholds for the 5 seeded rules' metrics | Medium | High | Catalog seeding is an F1 prerequisite. Surface gaps explicitly: rules without resolved threshold report `not_applicable` with `partial_reasons.missing_catalog_threshold = [metric_keys]` so the gap is visible, not silent. |
| F4a's foundations (org-chart, custom-field plumbing) are not yet in flight | High | Critical | F4a cannot be scheduled until those implementations begin. Track as explicit prerequisite — not as sub-tasks absorbed into F4a's estimate. F0–F3 unblocked in the meantime. |
| Foundation tracks (catalog, org-chart, person-domain API) have no owners and no open PRs (May 2026 audit) | High | Critical | Stop treating "PRD merged" as "in flight". F0b (catalog MVP) is now in diagnosis-layer scope; org-chart and person-domain API ownership must be escalated explicitly before F4a planning starts. If escalation fails, F4a is unbuildable on the documented timeline. |
| Tenant-isolation skip is broader than the single `handlers.rs:287` comment suggests (`query_metric()` also skips) | High | Critical | F0 mandates a one-time audit pass across `analytics-api` handlers for missing `tenant_id` injection (see F0 in-scope). Subsequent PRs are gated on the review checklist, but the existing surface area gets cleaned in one batch, not pull-by-pull. |
| Connector-side alias emission (github_login, bitbucket) has no owner — review-side rules are blocked indefinitely | High | High | Escalate ownership before F4a kickoff (Identity Open Q §6.9). Until at least one git source emits reviewer aliases, the rule library remains author-side-only and review-side `metric_key`s are AST-rejected with `not_supported_yet`. Communicate scope clearly to design partners. (Note: earlier framing also listed `slack_user_id` here. Slack is a different problem — admin API returns no user roster or messages, so even alias work would not unlock Slack-derived signals beyond the daily counters we already have.) |
| Exec-level Goodhart: `% Red teams` becomes a department KPI before §16.1 is decided | Medium | High | Close §16.1 before F2 ships; verdict roll-up policy precedes the technical capability to roll up. Until decided, `verdict_history` is per-team-cohort only at the API level — no roll-up endpoint exposed. |
| Catalog adds `kind=diagnosis` later, conflicts with our v1 `kind` reuse | Low | Medium | If we ship reusing `alert` and catalog later adds `diagnosis`, migration is mechanical: re-classify our diagnosis-authored rows to the new kind. Keep `source_app` / `created_by` columns to identify them. |
| LLM prompt injection leaks cross-tenant catalog | Low | Critical | Strict JSON output, tenant-filtered catalog injection, server-side `team_id` enforcement (§3.3.1). |
| Honest-NULL regressions cause false Red verdicts | Medium | High | `not_applicable` plumbed end-to-end; golden integration tests on each release. |
| Compliance escalation (GDPR Art. 22 / works councils in DE) | Medium | High | Verdicts strictly recommendational; full explanation on every flag; per-tenant ability to disable role profiles. |
| ClickHouse latency under tenant-wide aggregates | Medium | Medium | `*_bullet_rows` materialized views; per-rule batching; Redis cache (§10.1). |
| Role-catalog churn breaks historical verdicts | Medium | Medium | Versioned slugs; verdict_history pins `rule_version`; profile migration on rename. |

---

## 16. Open Questions

> **Note on owners.** Per project convention, open questions name **role-level owners** (PM, Legal, Engineering, identity-domain, etc.) and **milestone-based timing** (e.g. "before F2 ships"), not specific people or calendar dates. Owners change, documents stay. Each phase kickoff converts the role-level owner here into a named person on the kickoff agenda; that person is tracked in the phase's working doc, not in this PRD.

1.  **Verdict ownership at the org level**: when a team rolls up to a department, do we aggregate verdicts (worst-of?) or compute fresh at the department cohort? The math differs and the product implication differs. **Goodhart-above-the-manager risk**: there is internal precedent for executive leadership converting raw engineering signals (e.g. lines-of-code velocity claims) into KPIs they steer on. If `% of teams with Red verdict` becomes a department-level scoreboard before we have decided what that number means, we recreate the same trap one layer up. *Owner: PM. Milestone: decide before F2 kickoff — `verdict_history` makes department-level views technically possible, so the policy must lead the capability.*
2.  **Cross-tenant baseline opt-in**: would a customer ever benefit from "industry baselines" computed across tenants (anonymized)? Powerful but a compliance minefield. *Owner: PM + Legal. Default: no. Revisit post-GA only.*
3.  **IC visibility**: should ICs ever see flags about themselves (transparency) vs. manager-only (managerial tool)? Current PRD assumes manager-only — needs explicit user research before changing. *Owner: PM + Legal. Milestone: decide before F4a opens to first design partner.*
4.  **Rule marketplace**: should Tenant rules be shareable across tenants (template gallery)? Useful for cold-start but couples tenants. *Owner: PM. Defer until post-F7.*
5.  **Threshold confidence intervals**: P75 over a cohort of 8 has wide CIs. Do we expose CI to the manager, or wait until cohorts are large enough? *Owner: PM (decision); Engineering (implementation). Engineering preference: hide until cohort ≥20. Milestone: decide before F3 (relative rules).*
6.  **Drift acceptance UX**: when a profile is auto-flipped to `drift_review`, is the prior profile still active or paused? *Owner: PM. Default proposal: stays active until admin acts; flag the staleness in UI. Milestone: decide before F5 ships.*
7.  **Feedback weight on rule quality**: how much does a single manager's "false positive" tag move `false_positive_rate_30d`? Need a weighting model that resists single-noisy-user skew. *Owner: Engineering (modelling); PM (final scaling choice). Milestone: decide before F1 GA so feedback dashboard has a stable definition.*
8.  **`scope=Tenant` rules without role data**: should F1 allow tenant-wide rules at all (e.g. "any team with Red verdict for 4 weeks")? Or do tenant-scope rules wait until F4a so they can leverage HR-provided cohort dimensions? *Owner: PM. Engineering preference: allow only `team` and `team_relative` cohorts in F1. Milestone: decide before F1 kickoff.*
9.  **Metric Catalog `kind` registration for diagnosis layer**: catalog v1 defines `metric_threshold` with a `kind` enum that's DESIGN-owned, currently used for `{visual zones, alerting}`. Diagnosis layer is a third consumer: do we register a new `kind=diagnosis` (clean separation, more rows per metric) or reuse `kind=alert` / specific kinds like `warn`/`alert_trigger` (no schema change, but couples diagnosis policy to alerting policy)? *Owner: metric-catalog DESIGN authors (decision); diagnosis-layer Engineering (consumer feedback). Engineering preference: reuse existing kinds for v1 — fewer migrations — and propose `kind=diagnosis` only if a real conflict surfaces. Milestone: decide before F0b (catalog MVP) lands.*

Identity- and cohort-specific open questions live in Identity doc §6 (role inference buckets, connector custom-field whitelist, mapping drift, binding depth, predicate-coverage UX, predicate library evolution, onboarding HR sanity check).
