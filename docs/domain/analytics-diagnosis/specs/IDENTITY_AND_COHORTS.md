# Identity Resolution & Cohort Definition for Analytics Diagnosis

> Companion document to [`PRD.md`](./PRD.md). Split out because the role-handling and cohort-definition problem is the largest and most failure-prone part of the system. The main PRD assumes this contract and references it.

This document specifies:

1. Ground-truth state of identity resolution in the codebase today.
2. How people resolve to teams and how the author/reviewer namespace gap blocks review-side metrics.
3. The contract for cohort dimensions (slots), how they're populated per tenant via slot mapping, and how the self-install wizard wires it up.
4. How role data flows: HR-provided dimensions as the primary path, heuristics as fallback, formal taxonomy as a conditional future, eligibility predicates as the way rules express cohorts, and team-level mode for tenants without rich HR.
5. Risks and open questions that are specific to identity & cohorts.

Cross-references to the main PRD appear as "main §X.Y". Phase identifiers (F0, F1, …, F4a, I-C, I-D, I-E) are defined in main §14.

---

## TL;DR (one-pager)

> Read this first. The full doc is the deep-dive.

**The problem.** Diagnosis-layer rules need to ask "is this person in the right cohort, and did they do the expected work?" That sounds simple but explodes on contact with reality: HR data varies per tenant, people hold multiple roles, our connectors don't emit all the aliases we need, and the canonical domains we'd consume (org-chart, person-domain) don't have code yet.

**The model.** Two orthogonal axes:

*   `expected_functions` — **set per person** (composite roles native), **vocabulary per tenant** (no closed enum). HR-derived; falls back to role-text bucketing when normalization is absent.
*   `observed_activity` — per-function vocabulary of activity signals (commits, deals_closed, tickets_resolved, …). Tenant declares which sources feed which signal.

Rules compose them via a small predicate library: `function_eligible(F)`, `function_signal(F)`, `mismatch_into(F)`, `no_function_activity`. No privileged single-function flag (no `is_coder`).

**Slot mapping.** A per-tenant binding layer (`org_slot_mapping`) maps source fields → org-chart slots and function-signal bindings. Self-install wizard lets a tenant admin wire it up without Constructor onboarding involvement — *for slot bindings only*; per-person manual overrides today require a dbt seed PR (person-domain has no override API yet).

**Reality check (May 2026 audit).**

| What we depend on | Status |
|---|---|
| Email-keyed person resolution | Works (BambooHR + Cursor emit `email`) |
| Reviewer/git-host alias resolution | **Missing** — no connector emits `github_login` / `bitbucket_display_name` / `slack_user_id`; review-side rules blocked |
| `person_assignments` (org-chart) | **No code, no owner** — F4a is hard-blocked |
| Person-domain override API | **No code** — overrides happen via dbt seeds |
| BambooHR custom-fields ingestion (Map → typed silver) | **Flat** today; I-E.1 is real implementation work |

**What that means.** Diagnosis layer's plan must consume these contracts as they ship, not as they were promised to ship. F0–F3 only need email-keyed resolution (works). F4a needs everything in the table above to land first.

**What this doc does NOT own.** Alias resolution mechanics (identity-resolution PRD); golden-record assembly (person PRD); SCD2 hierarchies (org-chart PRD); calculation rules / `query_ref` (metric-catalog PRD). We consume those contracts.

**Reading guide.**

*   New to the problem: §0 (status of each domain), §4.3 (two-axis model), §3.1.1 (per-function activity signals).
*   Designing a rule: §4.3.1 (eligibility predicates), §3.1 (slot contract).
*   Implementing the wizard: §3.2 (slot mapping), §3.2.3 (configuration surface).
*   Risks and open questions: §5, §6.

---

## 0. Relationship to existing domain specs

This doc **extends** existing Insight domain specs, not replaces them. Where a contract already exists in the canonical domain PRDs, we reference it; we only specify what's genuinely new for the diagnosis layer.

**Implementation status (May 2026 audit, code-verified) — read before relying on any row below.** A merged PRD does not always mean code exists; an "implemented" service does not always mean its data flow is complete. Snapshot from a direct code/PR audit:

*   `identity-resolution` — **Tables exist; alias coverage incomplete; author/reviewer bridge structurally absent.**
    *   **Tables shipped:** `persons` + `account_person_map` (MariaDB, `insight/src/backend/services/identity/src/migration/m20260421_000001_persons.rs`); `identity.aliases` + `person.persons` (ClickHouse, `insight/src/ingestion/scripts/migrations/20260408000000_init-identity.sql`). Stable `person_id` (UUIDv7) achieved.
    *   **`value_type`s emitted today:** BambooHR (`email`, `employee_id`, `display_name`); Cursor (`email`, `id`, `display_name`). **Not found in code:** `github_login`, `bitbucket_display_name`, `slack_user_id`. This means review-side metrics (§2.1) and Slack-derived team detection (§3.2) cannot resolve to `person_id` today — the gap is **alias emission per connector**, not a missing bridge service.
    *   **`author_person_id` field exists** (m20260421_000001_persons.rs:98–101) but is not populated anywhere in current ingestion. So even when a git-host alias eventually lands, there is no code path that links a review observation to its author's golden record.
*   `person` — **ClickHouse table only (not Rust service); manual override = dbt seed PR.**
    *   `person.persons` is a ClickHouse table materialized from dbt seeds (`seed_persons_from_cursor.sql`, `seed_persons_from_claude_admin.sql`). There is **no Rust person-domain service**, no REST API, no admin UI for overrides.
    *   Golden-record fields `manager_person_id` and `org_unit_id` are defined in schema but **always default to `'00...0'`** in current seeds — they are reserved columns, not populated data.
    *   Implication: when this doc says "manual override via the canonical `*_source = 'manual'` flow per person PRD §5.2", the **canonical flow as code is a dbt seed PR**, not a tenant-admin-triggered API call. Self-install (§3.2.3) cannot rely on it without first building that API surface.
*   `org-chart` — **PRD-only, no code.** `person_assignments` / `org_units` tables do not exist; no open or in-flight PR found on `cyberfabric/insight`. F4a is hard-blocked on this work being scheduled and is not currently in any team's backlog.
*   `metric-catalog` — **PRD-only, no code.** Neither `metric_catalog` nor `metric_threshold` tables exist; `GET /catalog/metrics` endpoint is not implemented. **Today's reality**: thresholds live in the frontend (`insight-front/src/screensets/insight/api/thresholdConfig.ts`); backend `m20260422_000001_seed_metrics.rs` only seeds metric IDs and `query_ref`, not threshold values. Diagnosis layer cannot consume a catalog that does not exist — F0 phasing is revised in main §14.3 accordingly.
*   `connector` — **Custom-fields config flag exists; bronze flow is flat.** BambooHR `connector.yaml:44–50` accepts `bamboohr_employees_custom_fields` and concatenates them into the API field list, but values land in `raw_data` as top-level fields, **not** in a `Map(String, String)` column. No dbt unwrap to typed silver columns; no `/discovered-fields` endpoint. I-E.1 remains an implementation task, not a flag flip.
*   `ingestion` — **Implemented** (bronze/silver/gold pipeline, multiple connectors live).
*   **Tenant isolation** — `analytics-api/src/api/handlers.rs:287` MVP no-op confirmed in code. **Additional skip:** `query_metric()` (same file, ~line 246) does not inject `tenant_id` into the assembled SQL even though the metric row itself is filtered by tenant. So tenant-scoping today is partial-but-not-uniform; the "review checklist" framing in main §4.3 must explicitly cover the query-builder path, not just the entity load.

| Domain spec | Owns | Diagnosis layer reuses |
|---|---|---|
| `insight/docs/domain/identity-resolution/specs/PRD.md` | `aliases(person_id, alias_type, alias_value, …)`, bootstrap pipeline, hot-path person resolution from connector observations. Acknowledges author/reviewer gap (PRD.md:47–48) but does not solve it. | Person resolution from any alias. **Extends** with the author/reviewer namespace bridge (§2.2) — out of IR's current scope. |
| `insight/docs/domain/person/specs/PRD.md` | `persons` golden record: `id, display_name, email, username, role, manager_person_id, org_unit_id, location, completeness_score, conflict_status, status, *_source`. **Manual override** via `*_source = 'manual'` (PRD §5.2). Person availability (leave/capacity). | `role`, `manager_person_id`, `org_unit_id` golden-record fields. Manual-override mechanism — we **do not** introduce a separate `person_role_override` table; we use the canonical `*_source = 'manual'` flow. |
| `insight/docs/domain/org-chart/specs/PRD.md` | `org_units` (SCD Type 2 hierarchy), `person_assignments(person_id, assignment_type, org_unit_id \| assignment_value, effective_from, effective_to)`. Assignment types in v1: `org_unit, role, department, team, manager, project, location, cost_center`. | **Reuses `person_assignments`** for temporal slot historization — this is what an earlier draft of this doc called `class_org_membership`; that name is dropped in favour of the canonical `person_assignments`. New slots like `product`, `function`, `subteam`, `subdepartment`, `department_owner`, `function_owner` may require **new `assignment_type` values** added to the org-chart enum (coordinated across PRs — see §3.1). Per-function activity signals (§3.1.1) ride on a single new `assignment_type` (`function_signal_binding`) carrying tenant-specific bindings as JSON, rather than one `assignment_type` per function. |
| `insight/docs/domain/metric-catalog/specs/PRD.md` (v1 merged) | `metric_catalog(metric_key, label_i18n_key, unit, format, higher_is_better, source_tags, is_enabled, …)`. `metric_threshold(metric_key, scope, role_slug, team_id, good, warn, alert_trigger, alert_bad, is_locked, lock_reason, …)`. Scope ∈ `{product-default, tenant, role, team, team+role}`. Resolution: `team+role → team → role → tenant → product-default`, **locks act as ceilings**. `tenant_id IS NULL` enforced by CHECK in v1 (all metrics product-owned). `role_slug` and `team_id` are string references **without FK in v1**. Migration-only metadata writes. Calculation rules / `primary_query_id` deferred. | `metric_key` as the stable identifier on every rule. `role_slug` and `team_id` are the **canonical names** for our cohort dimensions — Identity doc §3.1 slots align with them. Threshold **values** come from `GET /catalog/metrics?role_slug=...&team_id=...` — diagnosis does not reimplement resolution. Rule **predicates** (cohort eligibility — §4.4) are orthogonal to thresholds and are this doc's concern. |
| `insight/docs/domain/connector/specs/PRD.md` | Bronze ingestion contract. **§5.2 explicitly requires** support for source-specific custom fields (BambooHR custom attributes) without core schema changes. | I-E.1 (BambooHR custom-field expansion in main §14.4) **is not a new requirement** — it's an existing contract. The diagnosis layer's job is the slot mapping layer (§3.2) on top, not the ingestion mechanism. |

### What this doc introduces that is genuinely new

1.  **Slot mapping (`org_slot_mapping`)** — a per-tenant binding layer that maps tenant-specific source field names to org-chart `assignment_type` values, plus the per-function activity-signal bindings (§3.1.1). Org-chart describes the *shape* of assignments; slot mapping decides *where the data comes from* for a given tenant. New (§3.2).
2.  **Eligibility predicates** — a small DSL for declaring rule cohorts (`function_eligible`, `function_signal`, `mismatch_into`, `no_function_activity`, …) that compose org-chart slots with per-function activity vocabularies. Metric-catalog has thresholds; nothing in any existing spec defines rule-cohort filtering. New (§4.3).
3.  **Source-monitoring of reviewer alias completeness** — diagnosis layer's bookkeeping on top of identity-resolution's `unmapped` queue (§2.1). Not a parallel bridge.
4.  **Self-install slot-mapping wizard** — none of the domain specs cover per-tenant configuration UX. Diagnosis layer owns it (§3.2.3).
5.  **Team-level first-class mode** — product framing for tenants without rich HR, not specified in any domain PRD (§4.4).

Everything else in this document — alias resolution, golden record, temporal assignments, custom-field ingestion, `metric_key` contract — **defers to the canonical domain spec**. If a contradiction surfaces, the canonical spec wins and this doc is wrong.

---

## 1. What diagnosis layer depends on (canonical sources)

The state of identity resolution in code is moving fast (PR #214 merged the MariaDB-backed identity store with append-only `persons` observations + `account_person_map` SCD2 cache; `person_id` is a stable UUIDv7). The canonical, current description lives in the identity-resolution PRD — we don't replicate it here.

What the diagnosis layer needs:

*   **Stable `person_id`** — UUIDv7, never re-derived. Source: identity-resolution PRD §5.
*   **Alias-based resolution** — connector observations (email, github_login, bitbucket display_name, etc.) all bridge to one `person_id`. This is the **mechanism that resolves the author/reviewer namespace gap by design** — provided each connector emits its observations with the right `value_type`. If a `value_type` is missing from a connector, the gap reappears for that source. Tracked in the identity-resolution PRD's bootstrap-pipeline phase, not here.
*   **Person→team membership over time** — owned by org-chart PRD via `person_assignments`.
*   **Tenant isolation** — currently no-op (`analytics-api/src/handlers.rs:287`). Built into every foundation PR as a review checklist (main §4.3) rather than a separate retrofit track.

Anything more detailed (alias schema, bootstrap modes, conflict resolution, golden-record assembly) is read from the canonical PRDs at evaluation time, not pinned here.

---

## 2. What diagnosis layer adds on top

### 2.1 Behaviour while reviewer aliases aren't fully ingested

**Today's gap, concretely** (from §0 audit): no connector emits `value_type='github_login'`, `bitbucket_display_name`, or `slack_user_id`. Author-side metrics work because BambooHR + Cursor emit `email`. Review-side metrics (`pr_review_time`, `reviews_given`, `time_to_first_review`) cannot resolve reviewers to `person_id` at all — `silver.fct_git_review.person_key` (github login) has no path into `person.persons`. The `author_person_id` column exists on `persons` but isn't populated by any current ingestion path.

This means **I-D in main §14.4 is not "size unknown, on someone else's roadmap"** — it is a concrete, missing piece of work whose owner is undefined. The review-side gap is the largest single content blocker for the diagnosis layer's rule library; we should treat it as such, not as a soft dependency.

Until alias emission ships per source:

*   AST validator rejects review-side `metric_key`s with `not_supported_yet` for that tenant.
*   We monitor which sources are alias-complete via identity-resolution's `unmapped` queue and lift the AST block per-source.

The bookkeeping above is correct; what's wrong in earlier framings of this doc is calling it "bookkeeping on top of identity-resolution's roadmap" — there is no roadmap item there yet. Diagnosis-layer planning must either own the connector-alias work or escalate it to be owned.

---

## 3. Cohort Dimensions & Slot Mapping

### 3.1 Slot contract (the dimensions we accept)

Based on a sample enterprise tenant with rich HR data, the slots the AST `cohort_filter` accepts:

| Slot | Example | Use in rules |
|---|---|---|
| `product` | Platform, Cloud, … | Cohort by product line — meaningful for cross-product comparisons. |
| `function` (set, tenant-declared) | `{R&D}`, `{Sales, Support}`, `{Engineering, PM}`, … | **Per-person set, not single value; vocabulary per tenant, not a fixed enum.** Used for tenant-wide rules and for selecting the activity vocabulary (§3.1.1). A person can belong to multiple functions simultaneously; rule cohorts use set-membership, not equality. |
| `division`, `department`, `subdepartment` | IT, Eng – Platform – IT, IT&Security | Hierarchy for drill-down and aggregation. |
| `team`, `subteam` | Security, Managers | True team membership — replaces today's heuristic team detection. |
| `job_title` | Junior Security Engineer, Vice President of Cloud | Freeform; secondary to the structured slots above. Heuristic-bucketing only applied if the structured slots are empty. |
| `reporting_to` | (manager person reference) | Already in `bronze_bamboohr` as `supervisor` — extends to verdict routing (whose digest does this person's flags appear in?). |
| `department_owner`, `function_owner` | (department/function lead person reference) | Natural recipients for tenant- and function-scoped verdict digests. |

**Note on `is_coder` (and similar single-function Bool flags).** Earlier drafts of this spec listed `is_coder` as a top-level slot. It has been **demoted out of the structured slot contract** — it was a hack specific to one function (engineering) under one HR convention (a Bool `Coder` field on the sample tenant), and it does not generalize. A sales tenant has no use for `is_coder`; promoting one function's flag to the slot vocabulary would force every other function to invent its parallel hack.

The replacement (§3.1.1 below) is **per-function activity signals**: each `function` declares which activity predicates are meaningful for it. `is_coder`-style signals become *one binding under a generic mechanism*, not a privileged slot of their own. Tenants that have a literal `Coder` Bool field still benefit — it just enters as one input to the generic `function_signal:engineering` predicate, not as a top-level dimension.

For tenants where these slots are not populated (small customers, or BambooHR instances not configured this way), the heuristic fallback in §4.2 applies for non-role slots only. For tenants where no slots can be populated at all, the team-level mode in §4.4 is the supported configuration.

#### 3.1.1 Per-function activity signals (generalisation of `is_coder`)

Every function-scoped diagnosis rule needs to answer two questions: *"is this person expected to do this kind of work?"* (role axis, §4.3) and *"did they actually do it in the period?"* (activity axis). The activity vocabulary is **per-function**, not universal — and the **set of functions itself is per-tenant, not a fixed enum**.

**Functions are tenant-declared, not enum-fixed.** Different companies have different function vocabularies. A B2B SaaS has `sales` / `cs` / `support` / `engineering` / `pm` / `design` / `marketing`. A consultancy has `delivery` / `practice` / `sales`. A fintech might split `engineering` into `platform` / `risk` / `data`. A startup might have `growth` instead of `marketing`. Hardcoding our shortlist would force every tenant into our taxonomy, which is exactly the trap we already fixed at the slot level.

The diagnosis layer therefore treats `function` as **an open string vocabulary the tenant declares** (subject to onboarding validation). Constructor ships a curated **starter library** of common functions with prebuilt activity signals — tenants reuse them when they fit, override or extend when they don't. The starter library (illustrative, *not normative*):

| Starter function | Example activity signals (when source is connected) |
|---|---|
| `engineering` | `commits_30d`, `prs_authored_30d`, `prs_reviewed_30d`, `incidents_resolved_30d` |
| `sales` | `deals_closed_30d`, `pipeline_added_$`, `calls_logged_30d`, `quota_attainment_pct` |
| `support` | `tickets_resolved_30d`, `csat_30d`, `first_response_time_p50` |
| `cs` | `qbrs_held_30d`, `health_score_movement`, `renewals_secured_30d` |
| `design` | `figma_files_authored_30d`, `design_reviews_given_30d` |
| `pm` | `prds_authored_30d`, `specs_reviewed_30d`, `roadmap_updates_30d` |
| `marketing` | `campaigns_launched_30d`, `content_published_30d` |
| `recruiting` | `interviews_conducted_30d`, `offers_extended_30d`, `hires_30d` |

A tenant's actual function vocabulary is whatever the tenant admin declares in F4a wizard or `org_slot_mapping` config. Tenants can rename (`marketing → growth`), split (`engineering → platform_eng + product_eng`), or invent (`legal_ops`, `community`). Constructor's only constraints are: (a) function names are stable strings (no auto-rename without admin action), (b) each declared function has a non-empty activity vocabulary or it cannot be referenced in rules, (c) the wizard validates that at least one declared function maps to ≥10% of active employees (otherwise the tenant is on team-level mode, §4.4).

**People can hold multiple functions simultaneously.** This is the second elasticity dimension:

*   **A founding engineer who also does PM**: declared functions = `{engineering, pm}`. They appear in both engineering and PM rule cohorts. Their `mismatch_into(F)` calculations only fire when activity is in a function *outside* their declared set.
*   **An SRE who handles customer escalations**: declared functions = `{engineering, support}`. Heavy support activity is *expected*, not a mismatch.
*   **A pure IC engineer**: declared functions = `{engineering}`. Heavy PM activity fires `mismatch_into(pm)`.

The mismatch predicate (§4.3.2) is redefined accordingly: mismatch fires when observed activity is heavy in a function **not in the person's declared function set**, not against a single "expected role". This handles composite roles cleanly without inventing weights or primary-function tie-breakers.

**Person → functions mapping** flows the same way as other slots: `org_slot_mapping` binds source fields to the function set. Common bindings:

*   BambooHR `Function` field → single function (most common case for traditional HR).
*   BambooHR `Function` + custom `Secondary Function` field → multiple functions.
*   `expected_role` → set of functions via tenant-declared role-to-functions table (e.g. `Founding Engineer → {engineering, pm}`).
*   Activity-derived inference (heuristic fallback in §4.2) when HR fields are silent.

**The contract.** Each declared function exposes a small named set of activity predicates. Rules and `function_signal(F)` predicates compose them. Across functions the *shape* is identical (Bool/numeric per person per period); the *vocabulary* differs by what data sources the tenant has connected for that function.

**Tenant configuration**. For each function the tenant declares, `org_slot_mapping` (§3.2) declares:

1.  Which silver/gold facts feed which activity predicate (e.g. `sales.deals_closed_30d ← bronze_salesforce.opportunity` filtered by `stage = "closed_won"`).
2.  Which HR fields, if any, *also* count as a hint for that function (e.g. a `Coder` Bool on the sample tenant feeds an `engineering` HR-hint — same mechanism, no longer a top-level slot).
3.  How `expected_role` strings map to function set membership (a person whose role is `Solutions Architect` may belong to `{engineering, sales}` per the tenant's declaration).

**Honest scoping**. F4a ships starter activity vocabularies for the functions where Constructor already has data sources (`engineering` via git is the only one currently certain — others depend on connector availability). The starter library above is *anchor*, not *commitment* — vocabularies become real per function only when the corresponding connectors land. Connector ownership is a separate track. Tenants can declare functions Constructor doesn't have a starter for; those functions live with HR-only signals (no activity vocabulary) until the tenant or Constructor wires data sources to them. This is honest degradation, not a blocker.

### 3.2 Per-tenant slot mapping (the binding layer)

**Problem**: Custom-field names differ per tenant. One tenant's `Coder` may be `is_developer` at another, or absent entirely. Some slots may live outside BambooHR (e.g. `team` derived from Slack channel membership). Hardcoding any one tenant's field names in the connector is wrong; ignoring custom fields at other tenants is also wrong.

**Solution**: A **slot-mapping configuration** binds the diagnosis layer's logical slots (§3.1) to concrete source fields per tenant.

#### 3.2.1 Schema

```rust
pub struct SlotMapping {
    pub tenant_id: TenantId,
    pub slot: Slot,          // team, function, product, function_signal_binding(F), ...
    pub bindings: Vec<Binding>,  // ordered: first non-NULL wins
}

pub struct Binding {
    pub source: Source,      // BambooHR, Slack, Heuristic, ManualOverride, Sheet, ...
    pub field: String,       // BambooHR custom-field name; Slack channel pattern; regex name; etc.
    pub value_type: ValueType, // Bool, String, Enum(Vec<String>)
    pub coercion: Option<Coercion>, // e.g. "Yes"/"No" → Bool true/false
}
```

Persisted in MariaDB (`org_slot_mapping`), one row per `(tenant_id, slot, source)` with an order field. The org-chart pipeline reads this config when building `person_assignments` (org-chart PRD): for each person, the resolver applies bindings in order and writes a row per `assignment_type` with the resolved value. Slots without an existing `assignment_type` (e.g. `product`, plus the generic `function_signal_binding` carrier from §3.1.1) require **new `assignment_type` values** added to the org-chart enum (§0).

#### 3.2.2 Resolution order (per-slot, per-person)

1.  **Manual override** — person-domain canonical override (`*_source = 'manual'` per person PRD §5.2) always wins. Diagnosis layer does not maintain a parallel override table. **Caveat from §0**: today the override mechanism is a dbt seed PR, not an API. Until a person-domain override API exists, F4a's "self-install wizard" cannot let a tenant admin override a person's role/team without Constructor onboarding involvement. This is called out as a real gap in §3.2.3 below, not handwaved.
2.  **Primary source** — first binding in the configured order. For an engineering tenant's `function_signal:engineering` HR-hint: BambooHR custom field `Coder` (Yes/No), coerced to Bool. For a sales tenant's `function_signal:sales` HR-hint: a `Quota Carrier` Bool, similarly coerced. Same mechanism, different binding.
3.  **Secondary sources** — fallback chain (e.g. Slack channel for `team` if BambooHR doesn't have it).
4.  **Heuristic** — regex on `job_title` (§4.2), only if explicitly enabled in the mapping.
5.  **Unmapped** — slot is `NULL` for this person; rules referencing it return `not_applicable` (main §4.2 honest-NULL).

#### 3.2.3 Configuration surface

The product is **self-install** — a Tenant Admin should be able to wire up Insight without Constructor's onboarding team in the loop. The mapping UI is **F4a core scope** but scoped pragmatically (wizard MVP → full mapper later):

*   **Self-install wizard MVP (F4a, day 1)** — BambooHR custom fields only:
    1.  Connector pulls BambooHR custom fields → `/discovered-fields` returns list with sample values.
    2.  Wizard suggests bindings based on field-name heuristics (`Team`/`Squad` → `team` slot; `Coder`/`Developer` Bool → `function_signal:engineering` HR-hint; `Quota Carrier`/`Sales Rep` Bool → `function_signal:sales` HR-hint; admin confirms or rejects each suggestion).
    3.  Coverage check runs live in-wizard (≥80% of active employees populated per slot); slots failing coverage are marked with a yellow warning but can still be saved.
    4.  Wizard ends with a "what works now" preview: which rules from the seeded library will be evaluable, which will stay `not_supported_yet`.
    5.  **If no fields match any logical slot** (small tenants, non-engineering-led HR setups): wizard does not enable F4a features. Tenant stays on F1/F2/F3 (team-level diagnosis) as a **first-class mode**, not a degraded fallback. Wizard explains this clearly: "your HR data doesn't currently expose role-level dimensions; you can still use Insight at team granularity." See §4.4.
*   **Out of MVP wizard scope** (F4a JSON config + admin UI in follow-up): non-BambooHR source bindings (Slack channels for `team`; heuristic-on-`job_title` for sparse fields). Available via `org_slot_mapping` JSON API in F4a; UI added post-launch.
*   **Permissions**: Slot-mapping CRUD is restricted to **Tenant Admin** role only. ICs and Team Leads see the resulting cohort labels but cannot edit bindings.
*   **API**: same operations exposed as REST for Constructor's onboarding team to use programmatically when assisting customers.
*   **Validation at save time**: coverage check (≥80% of active employees per slot), value-set check (e.g. a function-signal HR-hint binding must coerce to Bool), no orphan rules (rule references a slot or function that resolves to fully NULL → blocked at save with explanation).

**Honest scoping (from §0 audit).** The wizard can be self-install for **slot bindings** (since `org_slot_mapping` is a new table this PRD owns). It **cannot be self-install for per-person manual overrides** until a person-domain override API exists — today that's a dbt seed PR, owned by the ingestion repo. F4a's options:

*   **(a)** Build the person-override API as part of F4a (adds scope, lifts the calendar estimate).
*   **(b)** Ship the wizard without per-person overrides, accept that misclassified individuals require a Constructor-side dbt PR, and frame the wizard's "self-install" claim as scoped to *slot bindings only*.
*   **(c)** Defer F4a until person-domain owns its own service (no current owner — would block F4a indefinitely).

Recommend (b) for F4a v1: ship slot-mapping self-install, document the per-person override gap, escalate person-domain API ownership separately. Open question §6.8 (new) below.

#### 3.2.4 Binding composition (priority + AND)

A single source isn't always enough. Two composition modes:

*   **Priority list (default)**: ordered `Vec<Binding>`, first non-NULL wins. Use when one source is canonical with a fallback (`function_signal:engineering` HR-hint from BambooHR `Coder` field, falling back to `job_title` regex).
*   **All-of (AND)**: a slot resolves to `true` only when *every* binding evaluates truthy. Primary use case: tightening an HR-hint with an activity signal — e.g. `engineering_hr_hint = (BambooHR.Coder = Yes) AND (git.commits_30d > 0)`. Catches the common case of HR-marked coders who don't actually code; the equivalent for sales is `(BambooHR.QuotaCarrier = Yes) AND (calls_logged_30d > 0)`.

The binding model:

```rust
pub enum Binding {
    Field { source: Source, field: String, coercion: Option<Coercion> },
    First(Vec<Binding>),  // priority list — first non-NULL wins
    All(Vec<Binding>),    // AND — all must be truthy (Bool slots only)
}
```

Nesting is allowed but capped at depth 3 in F4a — pragmatic guardrail against config explosions, not a permanent limit.

#### 3.2.5 Change semantics (mapping change vs role change)

A diff in `person_assignments` for person P at time T can come from two unrelated events:

*   **Real-world change** — HR-source field value changed (P was promoted, joined a new team). Stable mapping; new value flowed in via the regular ingestion. This is naturally a main §9.4 "role transition" event — the person is pro-rated across the change date in cohort math.
*   **Mapping change** — admin re-wired the binding (e.g. `function_signal:engineering` HR-hint was bound to field `Coder`, now bound to `is_developer`). Same person can flip slot value with no real-world change.

Treat them differently:

*   **Real-world changes** are recorded inline in `person_assignments` history (`effective_from`/`effective_to`, per org-chart PRD). Verdict trends remain continuous; the person crosses cohort boundaries naturally.
*   **Mapping changes** are recorded in a separate `org_slot_mapping_history` table and emit a **trust break point** on the verdict trend chart (main F2). UI shows a small badge "mapping changed on YYYY-MM-DD" so trend regressions across the boundary aren't misread as real-world drift. Verdicts are **not** retroactively recomputed under the new mapping — the history pre-change reflects the rules-as-evaluated-then; the post-change uses the new mapping. This preserves auditability (main §6 GDPR) over silent rewrites.

Detection is mechanical: any `UPDATE` on `org_slot_mapping` is the "mapping change" signal. Source-field value changes flow through the normal silver build with no admin action.

#### 3.2.6 Connector requirement (I-E sub-task)

The BambooHR connector currently pulls a hardcoded list of 7 fields (`identity/src/people.rs:85-101`). To support arbitrary custom fields:

*   **Connector config**: accept a list of field names per tenant install (or fetch all custom fields by default; HR data volume is small).
*   **Field discovery**: `GET /discovered-fields` returns the list of BambooHR custom fields seen in the last sync, so the slot-mapping UI can populate dropdowns.
*   **Bronze schema**: store custom fields as `Map(String, String)` column on `bronze_bamboohr.employees` rather than per-field columns (avoids schema migration per tenant).

Two-phase fetch (Open Q §6.2): discovery (`/discovered-fields`) returns names + sample values for every custom field; production sync only pulls fields referenced in `org_slot_mapping`. Balances PII surface area with discovery UX.

---

## 4. Role Data Strategy

### 4.1 Roles come from the person golden record (the source)

Source-of-truth for roles is **`person.persons.role`** (and whatever stable role identifier the person/identity domain publishes alongside it). That's the source. We consume from there.

Today the field is **raw HR text** (e.g. "Senior Backend Engineer"), populated from BambooHR `jobTitle` via dbt seeds. There is no canonical role taxonomy yet, no `role_slug`, no normalization layer.

When normalization lands in the identity/person domain — whoever ends up implementing it, however the `role_alias`-style table is named — `person.persons` will expose a stable `role_slug` and we consume that automatically. Our consumer contract is with the person/identity domain.

What this means for diagnosis-layer work right now:

*   We do not build a parallel role taxonomy. We do not parse `job_title` ourselves into role buckets — not because someone forbids it, but because that would duplicate normalization that the person/identity domain should own.
*   Until normalization lands, role-based rules can only filter on the raw `role` value (string match, e.g. `role contains "Engineer"`). That's coarse but works for the dogfood tenant where Constructor's HR text is consistent. F4a's role features are *limited*, not blocked.
*   When stable `role_slug` arrives, rules switch from raw-text matching to slug equality with no AST schema change — `cohort_filter.role` is a string either way.

### 4.2 Other slots (`team`, `function`, `product`, function-signal HR-hints, etc.)

These are not roles. For the remaining cohort dimensions:

*   **Primary path** — HR-provided structured fields (e.g. `Teams`, `P&L Function`, `Coder` on a sample enterprise tenant) ingested via the BambooHR connector and bound to slots or function-signal HR-hints through `org_slot_mapping` (§3.2 + §3.1.1).
*   **Heuristic fallback** — allowed where structured fields aren't populated for a tenant. For `function_signal:engineering` HR-hint: `inferred_engineering = job_title matches /engineer|developer|sre|qa/i`. For `function_signal:sales`: `inferred_sales = job_title matches /sales|account|business development|bdr|sdr|ae\b/i`. For `team`: derive from Slack channel membership (configured per-tenant in §3.2).
*   **For `role` specifically**: no heuristic — see §4.1, we wait for normalization rather than duplicating it.

When a slot has no source binding for a tenant, the slot resolves to NULL → rules referencing it return `not_applicable` (main §4.2 honest-NULL).

### 4.3 Two orthogonal axes: *expected functions* and *observed activity*

Before the eligibility-predicate framing below, one shape must be made explicit: the diagnosis layer needs **two separate axes**, not one combined "is this person an engineer?" Bool. Both axes are *elastic* — vocabulary is per-tenant, and a person can hold multiple values on the role axis simultaneously.

| Axis | What it answers | Shape | Source |
|---|---|---|---|
| `expected_functions` | What functions is this person *supposed* to operate in? (engineering, PM, sales — possibly multiple) | **Set per person** (composite roles allowed) | HR / contract / org chart — `person.persons.role` plus tenant-declared role-to-functions table (§3.1.1) |
| `observed_activity` | What did this person *actually do* in the period? (shipped code, reviewed PRs, closed deals, resolved tickets, …) | Numeric/Bool per function per period | Silver/gold facts — git, jira, salesforce, zendesk, calendar |

These are **independent dimensions**, not interchangeable signals. Both are open-ended:

*   **Function vocabulary is tenant-declared** (§3.1.1) — Constructor's starter library is illustrative, not normative.
*   **A person belongs to a set of functions, not one** — `Founding Engineer` may declare `{engineering, pm}`; `Solutions Architect` may declare `{engineering, sales}`. Single-function people are the common case; the contract supports both. Earlier drafts collapsed them into a single OR-predicate (`hr_marked_coder OR shipped_code_90d`). That fold recreates the over-reliance trap from a different direction:

*   **A PM who ships code** matches an activity-OR-eligibility by activity. They get evaluated against engineering-IC rules (cycle time, review SLA, etc.) — wrong cohort, mismatched expectations, looks like a low-output engineer.
*   **An IC engineer on leave / new joiner** doesn't match by activity but should still be in the engineering cohort by role. OR catches them only if HR marked them — exactly the SPOF the over-reliance trap was about.
*   **A sales rep who never logs calls** disappears entirely under any "activity is enough" model — and that's a *signal*, not a hole to paper over.

**Real scenarios** that earlier framings made invisible: a PM who ships code, a developer who spends most of their cycle writing PRDs, a sales rep with no logged activity. A single Bool predicate erases all three. Set-semantics make them legible — for the PM-who-codes case, *expected* is `{pm}`, *observed* is engineering activity, the rule fires on `mismatch_into(engineering)`. The same mechanism applies to the sales rep with `expected_functions = {sales}` and no sales activity — `no_function_activity` fires.

**Composite roles are first-class.** A founding engineer with `expected_functions = {engineering, pm}` who ships code AND writes PRDs triggers no mismatch — both activities are expected. Heavy support activity from the same person *would* trigger `mismatch_into(support)`. This is the right semantics: the system asks "was this work expected?", not "is this person an engineer or a PM?".

**Implication for rule authoring:**

*   Function-IC rules filter on `function_eligible(F)` AND optionally `function_signal(F)` floors. Set-membership means a multi-function person is in *every* relevant cohort — not arbitrarily assigned to a "primary" one.
*   "Out-of-function activity" is a first-class diagnosis category: `mismatch_into(F)` flags people doing F-work without being declared for F; `no_function_activity` flags declared-but-silent. Both compose without engineering-special-cases or `X, Y` enumeration over function pairs.
*   Eligibility predicates (§4.3.1 below) compose the two axes explicitly via `function_eligible(F)` and `function_signal(F)` — activity is no longer mixed in at the eligibility layer, and the elastic function vocabulary means tenant-specific functions (`growth`, `legal_ops`, `community`) work without code change.

**Until `expected_role` is reliably populated** (depends on identity/person normalization landing — see §4.1), the diagnosis layer has only the raw `person.persons.role` string to work with. Coarse but functional: rules filter on raw role text using a small enum of known buckets per tenant. When normalization lands, the AST schema does not change.

#### 4.3.1 Cohort eligibility — explicit, composable, never hidden

**The over-reliance trap.** An earlier draft of this PRD treated an HR `is_coder` Bool as a privileged AST filter — auto-injected on every engineering rule, defaulting drill-downs, driving rebalance jobs. That was wrong, and the same mistake would repeat one-per-function if we added `is_seller`, `is_supporter`, `is_designer`. The general fix:

1.  **Definitional drift across tenants.** One tenant classifies a security engineer as `Coder=No`. Another tenant marks them `Yes`. The same problem repeats with `Quota Carrier`, `Active Sales Rep`, etc. Auto-injection makes verdicts depend on each tenant's HR convention — uninspectable, unnormalizable.
2.  **Single point of failure.** HR forgets to update the flag on transfer; person disappears from function rules silently.
3.  **Binary-trap.** Real life is fuzzy (EMs review PRs; SREs write infra code; AEs do CS work). A Bool flag erases nuance.
4.  **Hidden magic erodes trust.** A manager disputing a flag should see *why* a person is or isn't in the cohort. Silent auto-injection makes that hard.
5.  **Doesn't degrade gracefully.** Tenants without the function's flag fall back to "all employees" — worse than treating the flag as one signal among several.
6.  **Doesn't generalise across functions.** A privileged `is_coder` slot has no equivalent for sales / support / design without inventing parallel hacks. The mechanism must be function-agnostic from the start.

**The replacement.** Cohorts are composed from explicit, named **eligibility predicates** declared on each rule. No slot is privileged. No auto-injection.

#### 4.3.2 Eligibility predicates

A predicate is a small named expression evaluated per person, returning Bool. Predicates respect the two-axis model from §4.3: eligibility is *role-based*, activity floors are *separate predicates* the rule references when needed. Activity predicates use the **per-function vocabulary** from §3.1.1 — there is no privileged "is_coder"-style flag, and **`function` is a set, not a single value**.

Notation: `expected_functions` is the set of functions a person is declared to belong to (per §3.1.1). Predicates are written against set membership, not equality.

```sql
-- Role axis — set-membership eligibility (handles composite roles natively).
function_eligible(F)        := F ∈ expected_functions
function_eligible_any(Fs)   := expected_functions ∩ Fs ≠ ∅      -- person belongs to ANY of Fs
function_eligible_all(Fs)   := Fs ⊆ expected_functions          -- person belongs to ALL of Fs (rare)
manages_team                := slot.reporting_to_count > 0 OR expected_functions ∋ "management"

-- Activity axis — function_signal(F) is parameterised per declared function.
-- Each tenant binds it to function-specific facts via org_slot_mapping (§3.1.1).
function_signal(F)          := <tenant-bound expression in F's vocabulary>
                                -- e.g. F=engineering : commits_30d > 0 OR prs_authored_30d > 0
                                -- e.g. F=sales       : deals_closed_30d > 0 OR calls_logged_30d > 5
                                -- e.g. F=support     : tickets_resolved_30d > 0
                                -- F is whatever the tenant declared, including custom functions.

active_recently             := any_source.events_30d > 0
tenured_30d                 := person.hire_date <= now() - 30d

-- Cross-axis mismatch — fires when observed activity is heavy in a function NOT
-- in the person's declared set. Composite-role-aware by construction.
mismatch_into(F)            := function_signal(F) AND F ∉ expected_functions
                                -- e.g. mismatch_into(engineering)
                                --   true for a pure PM who ships code,
                                --   false for a {engineering, pm} founding engineer who ships code.

-- Function-eligible person showing no activity in any of their declared functions.
no_function_activity        := function_eligible_any(expected_functions)
                                AND NOT (∃F ∈ expected_functions : function_signal(F))
                                -- e.g. sales rep with no calls AND no deals AND no pipeline activity.
```

Predicates compose. Each seeded rule references predicates by name. The library starts with `function_eligible`, `function_eligible_any`, `function_eligible_all`, `function_signal`, `mismatch_into`, `no_function_activity`, plus a handful of universals (`active_recently`, `manages_team`, `tenured_30d`); admins can add more later. Function vocabularies are per-tenant bindings, not predicate definitions — the predicate library stays small and stable while the vocabulary grows with connectors and tenant declarations.

**Note on the old `mismatch(X, Y)` shape**: earlier draft defined `mismatch(X, Y) := function_eligible(X) AND function_signal(Y)` for distinct `X`, `Y`. That fails on composite roles (`{engineering, pm}` with PM activity would fire `mismatch(engineering, pm)` even though PM activity is expected). The replacement `mismatch_into(F) := function_signal(F) AND F ∉ expected_functions` is the correct generalisation: heavy activity outside any declared function is the signal, regardless of how many functions the person holds.

#### 4.3.3 No more single-axis OR-fold

Earlier drafts defined `engineering_eligible := shipped_code_90d OR hr_marked_coder`. That is **removed**. Activity is no longer mixed into eligibility, and eligibility is now function-parameterised (`function_eligible(F)`) so engineering is not privileged. Rationale in §4.3 — the canonical examples (PM who codes, dev who writes PRDs, sales rep with no activity) are invisible under any single-axis fold; making the two axes orthogonal across all functions makes the mismatch a first-class signal rather than an erasure.

When `expected_role` is unavailable (raw HR text only, no normalization yet), `function_eligible(F)` falls back to a string-match enum on the raw role text per tenant. Coarser but still role-axis, not activity-axis.

#### 4.3.4 Behaviour for non-eligible people

*   **Engineering rule cohorts**: a person is in the cohort iff the rule's predicate evaluates `true` for them. Non-eligible people are out-of-scope, not "missing" — verdict's `partial_reasons` does not mention them.
*   **Engineering drill-down**: cohort defaults to whatever the rule's predicate says. Eligibility is now role-based (§4.3) so drill-down shows people whose `expected_role` matched. A "Show all (incl. non-eligible)" toggle exposes the rest.
*   **Self-view (mandatory)**: a person sees their own metrics in full regardless of any predicate. Self-view is never gated.
*   **Leadership / tenant-wide rules** (e.g. "any team Red 4 weeks"): use predicates like `active_recently` or `manages_team`, not engineering eligibility. Predicate is part of the rule definition, not a magic global.

#### 4.3.5 Mapping coverage feedback (replaces "rebalance job")

A nightly job checks **predicate coverage** and **out-of-function activity** (now first-class — see §4.3). For each predicate and each declared function it reports:

*   **Total population matched** (raw count + % of active employees).
*   **Out-of-function activity**: counts of people firing `mismatch_into(F)` for each declared function `F`. These are not data-quality bugs to silence — they are organisational signals to surface (the "PM who codes" case lives here, as does "AE doing CS work").
*   **Declared-but-silent**: people firing `no_function_activity` — likely stale HR record, person on leave, or genuinely unclear assignment. Surfaces in the Tenant Admin queue **only above a threshold** (≥5 people **and** ≥5% of the matched population).
*   **Function vocabulary health**: per declared function — number of people in cohort, % with at least one populated activity signal, % of activity signals with no source binding. A function with <10% population OR no activity signal bound is flagged for review (likely declared but never wired).

The product still works correctly with mismatched data; the job helps admins keep HR clean, expose out-of-function work as a managerial signal, and prune unused function declarations.

#### 4.3.6 Why this fixes the over-reliance concern

| Concern | Before | After |
|---|---|---|
| Hidden auto-injection | `is_coder = true` injected on every engineering rule (and a parallel hack per function) | No auto-injection. Eligibility is `function_eligible(F)` over an elastic, tenant-declared function vocabulary. |
| HR-flag SPOF | Person with bad HR flag silently disappears from cohorts | Multiple binding sources per function (HR field, activity, role-text fallback); no single source is fatal. |
| Tenant definitional drift | Verdict semantics depend on tenant's `Coder` definition | Function vocabulary is per-tenant; semantics are about the tenant's declared functions, not Constructor's enum. |
| Closed function enum | Sales / support / growth / legal_ops cannot be expressed | Tenant declares its own functions; predicate library works against any string. |
| Composite roles invisible | `Founding Engineer` either misses one cohort or both | `expected_functions` is a set; multi-membership is native. |
| Tenants without the field | Everything falls back to "all employees" | Role-axis fallback uses `person.persons.role` text-bucketing per tenant. |
| Trust on dispute | "Why isn't X in this cohort?" hard to answer | Rule's predicate is visible; admin can read declared functions and bindings. |
| PM-who-codes / dev-who-PM-s / sales-rep-silent | OR-fold or single-function-Bool erases the mismatch | `mismatch_into(F)` and `no_function_activity` are first-class predicates over an elastic function vocabulary. |

### 4.4 Team-level mode (first-class, not degraded)

Some tenants will not have HR data rich enough for F4a cohorts (small companies; non-engineering-led HR setups; BambooHR instance configured minimally). The wizard recognises this and **does not enable F4a features** for them. The tenant stays on F1/F2/F3 functionality:

*   `scope=Team` rules — absolute and team-relative thresholds.
*   Team-level verdicts and verdict history.
*   Drill-down by team membership (already derived from git/Slack/Jira activity, no HR needed).
*   Snooze, feedback, the same `VerdictBanner`.

This is a **fully supported mode**, not a "limited trial". The product UI does not show empty F4a sections, doesn't push upgrade prompts, doesn't mark anything as "premium". Marketing framing: "Insight works at team granularity out of the box; richer HR data unlocks role-level diagnosis." Tenants can adopt F4a later by populating BambooHR or via JSON-config slot bindings.

This eliminates the "everything is broken" failure mode for tenants whose HR isn't ready — the most common failure mode for B2B analytics products.

---

## 5. Risks specific to identity & cohorts

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Inferred-role buckets misclassify >20% of a tenant's people | Medium | High | Per-tenant bulk override via person-domain manual-override flow; honest "(inferred)" labelling; cohort `min_size` makes bad buckets emit `not_applicable` rather than wrong verdicts. |
| Identity-domain role normalization never lands → roles stay as raw HR text indefinitely | Medium | Low | Diagnosis layer treats `person.persons.role` as a string regardless of normalization status. Coarser cohorts (e.g. "Senior Backend Engineer" vs "Backend Engineer" don't merge) but functional. F5 (LLM profiles) accommodates by widening fingerprint percentiles. |
| BambooHR `jobTitle` is too noisy / non-English on a new tenant | Medium | High | Per-tenant onboarding validation step (I-E gate); fallback to manual override import via person-domain manual-override flow. |
| Author/reviewer namespace bridge (I-D) never delivered | Medium | Medium | Review-side metrics rejected at AST validation with `not_supported_yet`; rule library trims to author-side only. Product is narrower but correct. |
| Slot-mapping misconfigured at onboarding → all rules `not_applicable` | Medium | High | Self-install wizard runs live coverage check; ≥80% threshold per slot; "what works now" preview before save; `mapping_binding_broken` alert in main §12 observability. |
| Tenant has BambooHR but no equivalent of a function-signal HR-hint (`Coder`, `Quota Carrier`, etc.) | High | Medium | No HR-hint field is fine — eligibility is on role-axis (`function_eligible(F)`), not on the HR-hint. The HR-hint is a *bonus signal* for mismatch detection, not a gate. When absent, mismatch detection is degraded but core function rules still work via role-text bucketing. |
| Tenant declares a custom function (`growth`, `legal_ops`, `community`) Constructor has no starter vocabulary for | High | Low | Tenant can declare any function; rules referencing it work as long as the tenant binds at least one activity signal. If no signal is bound, function-scoped rules return `not_applicable` (honest-NULL). Constructor adds starters opportunistically as patterns emerge across tenants. |
| Composite-role person (`{engineering, pm}`) gets evaluated only against one cohort | Medium | Medium | Eligibility uses set-membership (`function_eligible(F) := F ∈ expected_functions`); composite-role people are in every relevant cohort by construction. `mismatch_into(F)` only fires when F is *outside* the declared set. |
| Tenant declares too many functions / over-fragments their vocabulary | Medium | Medium | Wizard validation: declared functions with <10% population OR no activity signal bound → flagged for admin review. Function vocabulary health surfaced nightly (§4.3.5). |
| Reviewer / Slack / Bitbucket aliases never get emitted by connectors (I-D content) | High | High | No owner currently. Diagnosis layer must either escalate I-D ownership before F4a or accept the rule library is permanently author-side-only. AST validator rejects review-side rules with `not_supported_yet` — visible product-side, not a silent failure. |
| Person-domain override API never built → F4a wizard cannot offer per-person manual override self-install | High | Medium | Wizard scoped to slot bindings only; per-person overrides require Constructor onboarding-team-driven dbt PR. Documented in §3.2.3 option (b). Escalate ownership separately. |
| `expected_role` axis depends on identity-domain role normalization that has no owner | Medium | Medium | Fall back to per-tenant role-text enum (small, curated, ~10 buckets). Coarse but functional. AST schema unchanged when normalization lands. |
| Mapping change confused with real role change in verdict trends | Medium | Medium | `org_slot_mapping_history` lives in MariaDB alongside `org_slot_mapping`, distinct from the org-chart's `person_assignments` history; trend chart shows explicit break-point badge on mapping-edit dates (§3.2.5). |
| Out-of-function ICs feel surveilled when they appear in another function's dashboards | Medium | High (trust) | Out-of-function defaults to hidden in function-scoped drill-downs (§4.3.4); self-view always allowed; "Show all" toggle requires explicit click; flags never sent to ICs (manager-only — main §9.5). |
| Over-reliance on any single-function HR Bool (was `is_coder`; would repeat for `is_seller` etc.) as privileged gate | Medium | High | Removed auto-injection (§4.3). No function-specific privileged slot; replaced by generic `function_eligible(F)` and `function_signal(F)` (§3.1.1). HR-hints are one signal among several, function-symmetric. Tenant definitional drift no longer affects verdict semantics. |

---

## 6. Open questions specific to identity & cohorts

> **Note on owners.** Same convention as PRD §16 — open questions name **role-level owners** (PM, identity-domain, connector-owners, etc.) and **milestone-based timing** (e.g. "before F4a kickoff"), not specific people or calendar dates. Phase kickoffs convert role-level owners into named people on the kickoff agenda.

1.  **Role inference bucket list**: what ~10–15 buckets do we ship in the heuristic fallback (§4.2)? "Backend / Frontend / Mobile / SRE / QA / DataEng / PM / Designer / EM / Other" is a starting strawman. *Owner: PM (decision); Engineering (validation against dogfood-tenant data). Milestone: decide before F4a kickoff.*
2.  **Connector-side custom-field whitelist vs. fetch-all**: should the BambooHR connector pull *all* custom fields by default, or only those listed in the slot mapping? Fetch-all is simpler but stores fields we don't use (PII risk for arbitrary HR custom fields). *Owner: connector-owners (decision); diagnosis Engineering (consumer requirements). Engineering preference: discovery (`/discovered-fields`) fetches all field names + sample values; production sync fetches only the fields referenced in `org_slot_mapping`. Milestone: decide before I-E.1 kickoff.*
3.  **Mapping config drift detection**: when a BambooHR field is renamed, binding goes NULL → coverage drops below threshold → `mapping_binding_broken` observability alert fires (main §12). Affected rules auto-flip to `not_applicable` until admin re-binds. **Open sub-question**: should we auto-disable affected rules entirely (vs. emit `not_applicable` flood), and if so, after how long? *Owner: PM (policy). Milestone: decide before F4a ships.*
4.  **Binding composition depth limit**: F4a caps nesting at depth 3 (`First` of `All` of `Field`). Pragmatic, not principled. Revisit when a real tenant needs deeper. *Owner: Engineering. Revisit post-F4a launch.*
5.  **Predicate-coverage feedback UX**: §4.3.5 nightly job surfaces predicate-coverage and out-of-function activity. Surfaced only above ≥5-people-and-≥5% threshold. Resolution path TBD: per-person review? Bulk override? Edit predicate definition? *Owner: PM. Starting point: per-person review queue with the predicate definition visible. Milestone: decide before F4a UI ships.*
6.  **Predicate library evolution**: F4a ships ~6–8 named predicates. When does an admin gain the ability to author custom predicates (not just custom rules referencing existing predicates)? This is essentially a mini-DSL — adds power but also confusion surface. *Owner: PM (decision); Engineering (DSL design if approved). Engineering preference: defer to F6+ once Copilot exists; until then, predicate library is curated and shipped via migrations.*
7.  **Onboarding HR sanity check**: should every new tenant pass an "HR data sanity check" gate before any role-based rules activate? Validates per-slot coverage ≥80%, value-set sanity for booleans, no orphan rules. Trades onboarding friction for verdict trust. *Owner: PM (in absence of separate Customer Success function, onboarding is PM-run). Milestone: decide before F4a opens to first design partner.*

8.  **Person-domain override API ownership.** F4a wizard's "self-install" claim is partly false today (per §3.2.3): slot mappings are self-install, but per-person manual overrides require a dbt seed PR because no person-domain override API exists. **Open**: who owns building that API, and is it in scope for F4a or escalated separately? *Owner: identity-domain (where person-domain folded — natural home for golden-record CRUD). Diagnosis-layer Engineering escalates and consumes; does not own. Milestone: identity-domain decides scope before F4a kickoff; if API not in flight by then, F4a v1 ships with documented "slot bindings only" scope.*

9.  **Reviewer/Slack/Bitbucket alias emission ownership** (the real I-D). Per §0 audit, `value_type='github_login' / bitbucket_display_name / slack_user_id` are never emitted by any current connector. PRD's earlier framing called this "on identity-resolution's roadmap" — there is no such roadmap item. **Open**: who owns adding alias emission per source? *Owner: connector-owners per source (one PR per connector); identity-resolution as fallback owner if connector teams defer. Diagnosis-layer Engineering escalates; does not own. Milestone: at least one git source must be alias-complete before any review-side rule promotes to `active`.*

10. **`expected_functions` axis source.** Until identity/person-domain ships role normalization, the diagnosis layer needs a per-tenant **role-to-functions table** mapping raw `person.persons.role` text → set of declared functions (e.g. `Founding Engineer → {engineering, pm}`, `Solutions Architect → {engineering, sales}`, `Senior Backend Engineer → {engineering}`). **Open**: is this maintained by onboarding (per-tenant migration) or by the tenant admin (UI in F4a)? *Owner: PM (decision); onboarding (operational owner for design-partner phase); tenant admin (long-term owner once UI ships). Engineering preference: curated by onboarding for design partners, expose as JSON config in F4a, defer UI to post-F4a. Milestone: decide before F4a kickoff.*

11. **Function vocabulary governance.** With per-tenant elastic function vocabularies (§3.1.1), each tenant defines its own `function` set — `growth`, `legal_ops`, `community`, etc. **Open sub-questions**: (a) Is there a *recommended* shortlist surfaced in the wizard with one-click apply, or is the wizard fully blank? (b) Should activity signals from the starter library (e.g. `engineering` git signals) auto-attach when a tenant declares a function with that exact name, or always require explicit binding? (c) When a tenant renames a function mid-life, do existing rules referencing the old name auto-rewrite or break? *Owner: PM (a, c — product/policy); Engineering (b — implementation default). Engineering preference: (a) recommended shortlist with one-click apply but tenant can ignore; (b) auto-attach with admin confirmation in wizard; (c) explicit migration step — old name keeps working as alias for one verdict cycle, then breaks with clear error. Milestone: (a) and (b) decided before F4a wizard ships; (c) decided before second F4a tenant onboards (i.e. before rename actually possible in production).*

12. **Composite-role binding shape.** A person's `expected_functions` is a set, but tenants will most often have HR fields that produce single values (`Function = "Engineering"`). **Open**: do we require tenants to populate a `Secondary Function` custom field for composite roles, or do we derive multi-function membership from the role-to-functions table alone (so `Founding Engineer → {engineering, pm}` is a tenant-declaration, not a per-person field)? *Owner: PM (decision); Engineering (resolver implementation). Engineering preference: derive from role-to-functions table by default, allow per-person override via a `Secondary Function` field if the tenant has one. Avoids forcing custom HR fields just to support composite roles. Milestone: decide before F4a kickoff.*
