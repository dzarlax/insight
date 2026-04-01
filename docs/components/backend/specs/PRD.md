---
status: proposed
date: 2026-03-31
---

# PRD -- Backend

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Module-Specific Environment Constraints](#31-module-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Analytics](#51-analytics)
  - [5.2 Connector Management](#52-connector-management)
  - [5.3 Identity and Access Control](#53-identity-and-access-control)
  - [5.4 Cross-Source Identity Resolution](#54-cross-source-identity-resolution)
  - [5.5 Authentication](#55-authentication)
  - [5.6 Alerts](#56-alerts)
  - [5.7 Audit](#57-audit)
  - [5.8 Email](#58-email)
  - [5.9 Data Transformation](#59-data-transformation)
  - [5.10 Database Operations](#510-database-operations)
  - [5.11 Operational Health](#511-operational-health)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 NFR Inclusions](#61-nfr-inclusions)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [8.1 Analytics Consumption](#81-analytics-consumption)
  - [8.2 Connector Operations](#82-connector-operations)
  - [8.3 Identity Resolution](#83-identity-resolution)
  - [8.4 Access Control Configuration](#84-access-control-configuration)
  - [8.5 Alerting](#85-alerting)
  - [8.6 Compliance Audit](#86-compliance-audit)
  - [8.7 Platform Setup](#87-platform-setup)
  - [8.8 Transformation Monitoring](#88-transformation-monitoring)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Open Questions](#12-open-questions)
  - [OQ-BE-1: Bronze Write API ownership](#oq-be-1-bronze-write-api-ownership)
  - [OQ-BE-2: Collection runs tracking](#oq-be-2-collection-runs-tracking)
  - [OQ-BE-3: Permission model — ScopeGrants and SourceAccess](#oq-be-3-permission-model--scopegrants-and-sourceaccess)
  - [OQ-BE-4: Identity Resolution coordination](#oq-be-4-identity-resolution-coordination)
  - [OQ-BE-5: API Guidelines conformance](#oq-be-5-api-guidelines-conformance)
  - [OQ-BE-6: Incremental sync state persistence](#oq-be-6-incremental-sync-state-persistence)
  - [OQ-BE-7: Schema validation at ingestion boundary](#oq-be-7-schema-validation-at-ingestion-boundary)
  - [OQ-BE-8: Orchestrator migration — Kestra vs Argo Workflows](#oq-be-8-orchestrator-migration--kestra-vs-argo-workflows)
- [13. Risks](#13-risks)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

The Insight Backend is the API and business logic tier of the Insight platform. It serves analytics data from ClickHouse Silver and Gold layers, manages connector configurations and encrypted credentials, maintains organizational hierarchy imported from HR/directory systems (Active Directory, BambooHR, Workday, or similar), delivers business alerts, provides a compliance audit trail, and centralizes email delivery.

### 1.2 Background / Problem Statement

Organizations collect operational data across dozens of tools (version control, task trackers, collaboration, AI tools, HR systems) but lack a unified view of team performance, process bottlenecks, and AI adoption metrics. The ingestion layer (Airbyte, Kestra, dbt) extracts and transforms this data into ClickHouse. The backend must expose this data through secure, tenant-isolated, org-scoped APIs while giving administrators control over connector configurations, user roles, and alert thresholds.

The product is deployed as a standalone installation on customer Kubernetes clusters. It must not depend on any specific cloud provider, external secret manager, or bundled identity provider. Customers bring their own OIDC provider and HR/directory system (Active Directory, BambooHR, Workday, or similar).

### 1.3 Goals (Business Outcomes)

- Enable unit managers to view analytics metrics scoped to their organizational subtree with strict temporal boundaries on personnel transfers
- Provide self-service connector configuration so customers can onboard new data sources without vendor involvement
- Deliver proactive business alerts when key metrics cross configured thresholds, reducing time-to-awareness from days to minutes
- Maintain a queryable audit trail of all data access and configuration changes for compliance purposes
- Support multi-tenant data isolation so a single deployment can serve multiple organizational tenants

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Silver layer | Unified ClickHouse tables with standardized schemas across data sources |
| Gold layer | Aggregated ClickHouse tables with computed business metrics |
| Org unit | A node in the organizational hierarchy (team, department, division) |
| Follow-the-unit-strict | Visibility policy where data access follows org membership periods |
| Per-tenant encryption isolation | Security property ensuring that credential compromise in one tenant cannot expose credentials of another tenant |
| Golden Record | The canonical, deduplicated person record produced by identity resolution, representing a single real individual across all source systems |
| Workspace | The top-level tenant isolation boundary; all data, configuration, and access policies are scoped to a workspace |
| Connector | A configured integration with an external data source, managed through the Airbyte platform |
| Business Alert | A user-defined rule that monitors a metric against a threshold and triggers a notification when breached |

## 2. Actors

### 2.1 Human Actors

#### Viewer

**ID**: `cpt-insightspec-actor-viewer`

**Role**: End user who consumes dashboards and analytics within their org scope.
**Needs**: View dashboards, browse metrics, export data as CSV.

#### Analyst

**ID**: `cpt-insightspec-actor-analyst`

**Role**: Power user who creates and configures dashboards and chart visualizations.
**Needs**: All Viewer capabilities plus create, edit, and delete dashboard configurations and chart definitions.

#### Connector Administrator

**ID**: `cpt-insightspec-actor-connector-admin`

**Role**: Technical user responsible for configuring data source connectors and managing credentials.
**Needs**: Create, update, and delete connector configurations. Manage API keys and tokens. Trigger and monitor sync operations.

#### Identity Administrator

**ID**: `cpt-insightspec-actor-identity-admin`

**Role**: Administrator responsible for organizational structure and identity resolution.
**Needs**: Edit org tree, manage identity resolution rules, override person-to-identity mappings, trigger LDAP sync.

#### Tenant Administrator

**ID**: `cpt-insightspec-actor-tenant-admin`

**Role**: Top-level administrator with full control over tenant configuration.
**Needs**: All capabilities of other roles plus manage role assignments, configure notification rules, provision new tenants.

### 2.2 System Actors

#### OIDC Provider

**ID**: `cpt-insightspec-actor-oidc-provider`

**Role**: Customer's existing identity provider that issues JWT tokens for authentication.

#### HR/Directory System

**ID**: `cpt-insightspec-actor-hr-directory`

**Role**: Customer's HR or directory system (Active Directory via LDAP, BambooHR via API, Workday via API, or similar) that provides organizational hierarchy and person records. The Identity Service supports pluggable adapters for different source systems.

#### Airbyte

**ID**: `cpt-insightspec-actor-airbyte`

**Role**: Data extraction platform that manages connector syncs. The backend interacts with its API for connection management and sync triggering.

#### SMTP Server

**ID**: `cpt-insightspec-actor-smtp-server`

**Role**: Customer's email server used for delivering alert notifications and operational emails.

## 3. Operational Concept & Environment

### 3.1 Module-Specific Environment Constraints

- Deployed on Kubernetes (1.27+) via Helm chart
- All required infrastructure is bundled and deployed alongside the Backend
- No dependency on cloud-provider-specific services
- Authentication exclusively via customer OIDC provider
- Organizational structure sourced from customer HR/directory system via pluggable adapters (AD/LDAP, BambooHR API, Workday API)

## 4. Scope

### 4.1 In Scope

- Analytics read API over Silver and Gold layers with OData filtering
- Metrics catalog management (CRUD for metric definitions)
- Dashboard and chart configuration management
- CSV data export with temporary object storage
- Connector configuration management via Airbyte API
- Credential management with per-tenant encryption isolation
- Org tree sync from HR/directory systems via pluggable adapters
- OIDC-to-person identity resolution (login mapping)
- Cross-source identity resolution (alias matching, golden records, merge/split)
- RBAC with five roles (Viewer, Analyst, Connector Admin, Identity Admin, Tenant Admin)
- Org-tree-based data visibility with follow-the-unit-strict policy
- Data transformation configuration and execution orchestration with observable status
- Business alerts on metric thresholds with email notifications
- Append-only audit trail
- Centralized email delivery service
- Forward-only database migrations for continuous deployment
- Operational monitoring and alerting
- Health and readiness endpoints for critical dependencies

### 4.2 Out of Scope

- Tenant onboarding wizard (future -- initial tenant seeded via Helm values)
- Dashboard sharing across users or org units (future v2)
- Circuit breaker pattern (future v2 -- retry with backoff is sufficient for v1)
- GDPR data deletion workflows (future -- schema designed to not preclude it)
- Custom report scheduling (future -- CSV export is manual in v1)
- PDF report generation (future v2)
- Public analytics API (future v2 -- external API for customers to query analytics data programmatically, build custom integrations, and process metrics outside the bundled frontend; v1 exposes internal APIs consumed only by the bundled React SPA)
- Frontend implementation (separate PRD)

## 5. Functional Requirements

### 5.1 Analytics

#### Analytics Query Execution

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-analytics-read`

The system **MUST** execute read queries against ClickHouse Silver and Gold tables with OData-style filtering, sorting, pagination, and field projection, scoped to the requesting user's visible org units and membership time ranges.

**Rationale**: Core product value -- users need to access analytics data within their authorized scope.

**Actors**: `cpt-insightspec-actor-viewer`, `cpt-insightspec-actor-analyst`

#### Metrics Catalog Management

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-metrics-catalog`

The system **MUST** provide CRUD operations for metric definitions (name, description, unit, formula reference, category).

**Rationale**: Metrics must be discoverable and described for dashboard builders and analysts.

**Actors**: `cpt-insightspec-actor-analyst`, `cpt-insightspec-actor-tenant-admin`

#### Dashboard Configuration

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-dashboard-config`

The system **MUST** provide CRUD operations for dashboard and chart configurations (chart type, metric references, dimensions, filters).

**Rationale**: No-code dashboard building requires persistent chart configurations.

**Actors**: `cpt-insightspec-actor-analyst`

#### CSV Data Export

- [ ] `p2` - **ID**: `cpt-insightspec-fr-be-csv-export`

The system **MUST** allow users to trigger CSV exports of query results, store exports on S3-compatible storage, return a download link, and auto-expire exports after one week.

**Rationale**: Users need to export data for offline analysis and reporting.

**Actors**: `cpt-insightspec-actor-viewer`, `cpt-insightspec-actor-analyst`

### 5.2 Connector Management

#### Connector Configuration

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-connector-crud`

The system **MUST** provide CRUD operations for connector configurations (source type, parameters, schedule) and manage Airbyte connections via the Airbyte API (create, update, trigger sync, delete).

**Rationale**: Customers must be able to configure and manage their data sources without vendor involvement.

**Actors**: `cpt-insightspec-actor-connector-admin`

#### Credential Management

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-secret-management`

The system **MUST** securely store connector credentials with per-tenant isolation. Credential compromise in one tenant **MUST NOT** affect other tenants.

**Rationale**: API keys and tokens are sensitive. Per-tenant isolation limits blast radius of key compromise.

**Actors**: `cpt-insightspec-actor-connector-admin`

### 5.3 Identity and Access Control

#### Org Tree Synchronization

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-org-tree-sync`

The system **MUST** automatically synchronize the organizational hierarchy from customer HR/directory systems on a configurable schedule, maintaining person-org membership records with temporal validity (effective_from, effective_to). The system **MUST** support pluggable source adapters (Active Directory via LDAP, BambooHR via API, Workday via API) so customers can use their existing HR infrastructure.

**Rationale**: Org-based data visibility requires an up-to-date org tree that tracks membership history. Different customers use different HR/directory systems.

**Actors**: `cpt-insightspec-actor-hr-directory`, `cpt-insightspec-actor-identity-admin`

#### Identity Resolution

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-identity-resolution`

The system **MUST** map OIDC subject claims to internal person records to establish a stable internal identity for each authenticated user. On first login, the mapping **MUST** be created automatically.

**Rationale**: OIDC sub claims are opaque and IdP-specific. The system needs a stable internal person_id.

**Actors**: `cpt-insightspec-actor-oidc-provider`

#### Role-Based Access Control

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-rbac`

The system **MUST** enforce role-based access control with five roles (Viewer, Analyst, Connector Admin, Identity Admin, Tenant Admin). Each role **MUST** grant a defined set of permissions. Roles **MUST** be assignable per-tenant per-user by Tenant Administrators.

**Rationale**: Different users need different levels of access to platform features.

**Actors**: `cpt-insightspec-actor-tenant-admin`

#### Org-Based Data Visibility

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-visibility-policy`

The system **MUST** enforce follow-the-unit-strict visibility: when a person transfers between org units, the previous manager sees metrics only before the transfer date and the new manager sees metrics only from the transfer date onward. Unit members **MUST** see only their own unit's data.

**Rationale**: Prevents data leakage across organizational boundaries even for historical data.

**Actors**: `cpt-insightspec-actor-viewer`, `cpt-insightspec-actor-analyst`

### 5.4 Cross-Source Identity Resolution

#### Identity Resolution Service

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-identity-resolution-service`

The system **MUST** map disparate identity signals (emails, usernames, employee IDs, system-specific handles) from multiple source systems into canonical person records. The system **MUST** support conflict detection for ambiguous matches and manual merge and split operations with audit trail. See [Identity Resolution DESIGN](../../domain/identity-resolution/specs/DESIGN.md) and [Backend DESIGN section 3.2](./DESIGN.md) for implementation details.

**Rationale**: Cross-source analytics (correlating a person's Git commits with their Jira tasks, calendar events, and HR data) requires a single canonical person_id across all data sources. Without identity resolution, each source has its own user identifiers that cannot be joined.

**Actors**: `cpt-insightspec-actor-identity-admin`, `cpt-insightspec-actor-tenant-admin`

### 5.5 Authentication

#### OIDC Authentication

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-oidc-auth`

The system **MUST** authenticate all API requests via OIDC/JWT tokens issued by the customer's identity provider. No bundled identity provider or user/password management **MUST** be included.

**Rationale**: Enterprise customers have existing IdPs. The product must integrate, not replace.

**Actors**: `cpt-insightspec-actor-oidc-provider`

### 5.6 Alerts

#### Business Alerts

- [ ] `p2` - **ID**: `cpt-insightspec-fr-be-business-alerts`

The system **MUST** allow users to define alert rules (metric, threshold, comparison operator, evaluation interval, recipients). The system **MUST** periodically evaluate thresholds against ClickHouse data and send email notifications when thresholds are crossed. Alert rules **MUST** respect org-tree visibility.

**Rationale**: Proactive notifications reduce time-to-awareness for process degradation.

**Actors**: `cpt-insightspec-actor-analyst`, `cpt-insightspec-actor-tenant-admin`

### 5.7 Audit

#### Audit Trail

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-audit-trail`

The system **MUST** log all data access, configuration changes, secret access, and authentication events as structured audit events. The audit trail **MUST** be queryable with OData filtering and stored in ClickHouse with configurable retention.

**Rationale**: Compliance requires knowing who accessed what data, when, and what was changed.

**Actors**: `cpt-insightspec-actor-tenant-admin`

### 5.8 Email

#### Centralized Email Delivery

- [ ] `p2` - **ID**: `cpt-insightspec-fr-be-email-delivery`

The system **MUST** provide a centralized email delivery service that renders templates, delivers via SMTP with retry logic, and tracks delivery status. No other backend component **MUST** interact with SMTP directly.

**Rationale**: Centralizing email avoids SMTP configuration duplication and enables unified retry, rate-limiting, and template management.

**Actors**: `cpt-insightspec-actor-smtp-server`

### 5.9 Data Transformation

#### Transform Rules Management

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-transform-rules`

The system **MUST** provide managed transformation configuration and execution orchestration with observable status. The system **MUST** allow administrators to define how data from multiple connectors is merged into unified tables and how unified data is aggregated into metric tables. The system **MUST** manage dependencies between connectors and transforms so that transformation runs execute after relevant syncs complete, and **MUST** expose transformation run status (last run time, duration, success/failure, error details).

**Rationale**: Transforms are cross-source logic (merging multiple connectors into unified schemas) and cannot be managed per-connector. A dedicated management surface enables administrators to configure transformation rules and monitor execution without direct access to orchestration tooling.

**Actors**: `cpt-insightspec-actor-tenant-admin`, `cpt-insightspec-actor-connector-admin`

### 5.10 Database Operations

#### Forward-Only Database Migrations

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-forward-only-migrations`

The system **MUST** manage database schema through versioned, forward-only migrations that execute automatically during deployment. Rollback migrations **MUST NOT** exist. Every migration **MUST** be backward-compatible with the previous application version so that rolling deployments can run old and new code against the same schema simultaneously. Destructive schema changes (column drops, table drops) **MUST** be deferred to a subsequent migration after the old code is fully decommissioned. Migrations **MUST** be idempotent -- re-running a migration that has already been applied **MUST** be a no-op.

**Rationale**: Forward-only migrations are critical for continuous deployment. Rollback scripts create a false sense of safety -- in practice they are rarely tested, often fail on production data, and introduce risk of data loss. Instead, a broken migration is fixed by shipping a new forward migration. This approach enables zero-downtime rolling deployments where old and new pod versions coexist during rollout.

**Actors**: This is a system-level operational concern. Migrations execute automatically as part of the deployment process without human actor involvement.

### 5.11 Operational Health

#### Health and Readiness Endpoints

- [ ] `p1` - **ID**: `cpt-insightspec-fr-be-health-check`

The system **MUST** expose health and readiness endpoints reporting status of all critical dependencies (databases, caches, message brokers, external services). The health endpoint **MUST** distinguish between liveness (the process is running) and readiness (the service can handle requests). The readiness endpoint **MUST** return degraded status when any critical dependency is unreachable.

**Rationale**: Kubernetes liveness and readiness probes depend on health check endpoints. Without them, the platform cannot automatically restart failed instances or exclude unhealthy instances from load balancing. On-premises deployments require self-diagnostic capability since customers may not have deep observability tooling.

**Actors**: `cpt-insightspec-actor-tenant-admin`

## 6. Non-Functional Requirements

### 6.1 NFR Inclusions

#### Tenant Data Isolation

- [ ] `p1` - **ID**: `cpt-insightspec-nfr-be-tenant-isolation`

The system **MUST** isolate tenant data at the application layer across all storage and messaging systems. A query from tenant A **MUST NOT** return data belonging to tenant B under any circumstances.

**Threshold**: Zero cross-tenant data leaks.

**Rationale**: Multi-tenant deployment requires strict data boundaries.

#### Query Safety

- [ ] `p1` - **ID**: `cpt-insightspec-nfr-be-query-safety`

All database queries **MUST** be safe from injection attacks. Query timeouts **MUST** be enforced per request.

**Threshold**: Zero SQL injection vectors in query builder code.

**Rationale**: OData-to-SQL translation is a high-risk injection surface.

#### Secret Isolation

- [ ] `p1` - **ID**: `cpt-insightspec-nfr-be-secret-isolation`

Compromise of one tenant's data encryption key **MUST NOT** expose secrets of other tenants. Key rotation for one tenant **MUST NOT** require re-encryption of other tenants' data.

**Threshold**: Per-tenant blast radius containment.

**Rationale**: Shared deployment means defense-in-depth for credential storage.

#### Rate Limiting

- [ ] `p2` - **ID**: `cpt-insightspec-nfr-be-rate-limiting`

The system **MUST** enforce per-route rate limiting (requests per second, burst, max in-flight) on all API endpoints with configurable defaults.

**Threshold**: 429 response returned before service degradation occurs.

**Rationale**: Prevents one tenant or user from impacting service availability for others.

#### Graceful Shutdown

- [ ] `p2` - **ID**: `cpt-insightspec-nfr-be-graceful-shutdown`

The system **MUST** ensure zero message loss during rolling deployments. On shutdown signal, the system **MUST** stop accepting new requests, drain in-flight requests, commit event stream offsets, and close database connections before exiting.

**Threshold**: Zero message loss during rolling deployments.

**Rationale**: Standalone product with customer SLAs requires zero-downtime deployments.

#### Retry Resilience

- [ ] `p2` - **ID**: `cpt-insightspec-nfr-be-retry-resilience`

All retryable operations **MUST** use exponential backoff with jitter. Client errors (4xx) and permanent failures **MUST NOT** be retried. Each retry **MUST** emit a warning log with attempt number and delay.

**Threshold**: Recovery within retry budget (3-5 attempts depending on operation).

**Rationale**: Downstream dependencies (ClickHouse, MariaDB, LDAP, Airbyte, SMTP) will have transient failures.

#### API Versioning

- [ ] `p2` - **ID**: `cpt-insightspec-nfr-be-api-versioning`

Every service **MUST** expose versioned API endpoints (`/api/v1/...`) from day one. Older API versions **MUST** continue working during rolling updates.

**Threshold**: Zero breaking changes to v1 endpoints without v2 migration path.

**Rationale**: Standalone product deployed to customer environments cannot force-upgrade clients.

### 6.2 NFR Exclusions

- **Horizontal ClickHouse sharding**: Not required for v1. Vertical scaling sufficient for expected data volumes. Revisit when single-node capacity is exceeded.
- **Distributed tracing (OpenTelemetry)**: Out of scope for v1. Structured logging with correlation_id provides sufficient debugging capability initially.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Analytics API

- [ ] `p1` - **ID**: `cpt-insightspec-interface-analytics-api`

**Type**: REST API

**Stability**: stable

**Description**: Read API for analytics queries, metrics catalog, dashboard configurations, and CSV exports.

**Breaking Change Policy**: Major version bump required for breaking changes. V1 endpoints maintained until V2 is stable.

#### Connector Manager API

- [ ] `p1` - **ID**: `cpt-insightspec-interface-connector-api`

**Type**: REST API

**Stability**: stable

**Description**: CRUD API for connector configurations, credential management, and sync operations.

**Breaking Change Policy**: Major version bump required for breaking changes.

#### Identity Service API

- [ ] `p1` - **ID**: `cpt-insightspec-interface-identity-api`

**Type**: REST API

**Stability**: stable

**Description**: Read API for org tree, person details, role management, and LDAP sync triggers.

**Breaking Change Policy**: Major version bump required for breaking changes.

#### Alerts Service API

- [ ] `p2` - **ID**: `cpt-insightspec-interface-alerts-api`

**Type**: REST API

**Stability**: stable

**Description**: CRUD API for alert rules and alert history.

**Breaking Change Policy**: Major version bump required for breaking changes.

#### Identity Resolution Service API

- [ ] `p1` - **ID**: `cpt-insightspec-interface-identity-resolution-api`

**Type**: REST API

**Stability**: stable

**Description**: API for managing resolved persons (golden records), aliases, merge/split operations, conflict resolution, and bootstrap job triggers.

**Breaking Change Policy**: Major version bump required for breaking changes.

#### Transform Service API

- [ ] `p1` - **ID**: `cpt-insightspec-interface-transform-api`

**Type**: REST API

**Stability**: stable

**Description**: CRUD API for transformation rules, metric table rules, transform dependency graph, and run triggers.

**Breaking Change Policy**: Major version bump required for breaking changes.

#### Audit Service API

- [ ] `p2` - **ID**: `cpt-insightspec-interface-audit-api`

**Type**: REST API

**Stability**: stable

**Description**: Read-only API for querying the audit trail with OData filtering.

**Breaking Change Policy**: Major version bump required for breaking changes.

### 7.2 External Integration Contracts

#### Airbyte API Contract

- [ ] `p1` - **ID**: `cpt-insightspec-contract-airbyte`

**Direction**: required from client (Connector Manager calls Airbyte API)

**Protocol/Format**: HTTP/REST

**Compatibility**: Depends on Airbyte API version. Connector Manager abstracts Airbyte API details.

#### HR/Directory Source Contract

- [ ] `p1` - **ID**: `cpt-insightspec-contract-hr-directory`

**Direction**: required from client (Identity Service queries org source via pluggable adapter)

**Protocol/Format**: LDAP/LDAPS (Active Directory, OpenLDAP) or HTTP/REST (BambooHR API, Workday API)

**Compatibility**: Adapter-based -- each adapter implements a common interface for org tree and person data retrieval.

#### Kestra API Contract

- [ ] `p1` - **ID**: `cpt-insightspec-contract-kestra`

**Direction**: required from client (Transform Service triggers transformation runs via Kestra API)

**Protocol/Format**: HTTP/REST

**Compatibility**: Depends on Kestra API version. Transform Service abstracts Kestra API details.

#### SMTP Contract

- [ ] `p2` - **ID**: `cpt-insightspec-contract-smtp`

**Direction**: required from client (Email Service delivers via SMTP)

**Protocol/Format**: SMTP (port 587, STARTTLS)

**Compatibility**: Standard SMTP. Customer provides server.

## 8. Use Cases

### 8.1 Analytics Consumption

#### Dashboard Data Retrieval

- [ ] `p1` - **ID**: `cpt-insightspec-usecase-view-dashboard`

**Actor**: `cpt-insightspec-actor-viewer`

**Preconditions**:
- User is authenticated via OIDC
- User has Viewer role or higher with at least one org-unit scope grant
- Connectors are configured and syncing data for the relevant sources

**Main Flow**:
1. Viewer opens a dashboard in the Frontend
2. Frontend sends analytics query to the Backend Analytics API with the user's access token
3. Backend validates the OIDC token and resolves the user's role and org-unit scope grants
4. Backend constructs the analytics query with automatic scope filters limiting results to the granted org units and membership time ranges
5. Backend executes the query against ClickHouse
6. Backend returns the filtered, aggregated results to the Frontend
7. Frontend renders charts

**Postconditions**: Viewer sees analytics data for only the people in their granted org units and membership periods. An audit event is recorded for the data access.

**Alternative Flows**:
- **Expired token**: If step 3 detects an expired token, the Backend returns a 401 response. The Frontend redirects to the OIDC provider for re-authentication.
- **No scope grants**: If step 3 finds no org-unit scope grants for the user, the Backend returns an empty result set with a 200 response (not an error -- the user is authenticated but has no data visibility).
- **ClickHouse unavailable**: If step 5 fails due to data store unavailability, the Backend returns a 503 response with a retry-after header.

#### Analytics Data Export

- [ ] `p2` - **ID**: `cpt-insightspec-usecase-analytics-export`

**Actor**: `cpt-insightspec-actor-analyst`

**Preconditions**:
- User is authenticated with Analyst role or higher
- User has org-unit scope grants covering the target data

**Main Flow**:
1. Analyst selects a date range and metric set for export
2. Frontend sends export request to the Backend Analytics API
3. Backend validates authorization and scope
4. Backend executes the query with RBAC filters applied
5. Backend formats results as CSV, stores on S3-compatible storage, and returns a download link
6. Analyst downloads the file for offline analysis

**Postconditions**: Export file contains only data within the user's authorized scope. Audit event records the export operation including row count and filters applied.

**Alternative Flows**:
- **Large result set**: If the query would return more than 100,000 rows, the Backend returns a 413 response suggesting the user narrow the date range or scope.

### 8.2 Connector Operations

#### Onboarding a New Data Source

- [ ] `p1` - **ID**: `cpt-insightspec-usecase-configure-connector`

**Actor**: `cpt-insightspec-actor-connector-admin`

**Preconditions**:
- User is authenticated with Connector Admin role
- Airbyte is reachable
- The target source system's API credentials are available

**Main Flow**:
1. Connector Admin selects the connector type (e.g., GitHub, Jira, BambooHR) from available templates
2. Connector Admin provides connection parameters (API URL, credentials, sync scope)
3. Backend validates the connection parameters by performing a test connection to the source system
4. Backend encrypts credentials with tenant-scoped encryption and creates the Airbyte connection via API
5. Backend records the connector creation in the audit trail
6. Connector Admin triggers an initial sync
7. Connector Admin monitors sync progress through the connector status API

**Postconditions**: New connector is configured and actively syncing. Credentials stored encrypted. Audit trail records who created it and when.

**Alternative Flows**:
- **Invalid credentials**: If step 3 fails the test connection, the Backend returns a validation error with the specific failure reason (authentication failed, endpoint unreachable, insufficient permissions). No connector is created.
- **Airbyte unreachable**: System retries with backoff, returns 503 after max attempts.
- **Duplicate connector**: If a connector for the same source instance already exists, the Backend returns a conflict error.

### 8.3 Identity Resolution

#### Reviewing and Resolving Identity Conflicts

- [ ] `p1` - **ID**: `cpt-insightspec-usecase-identity-review`

**Actor**: `cpt-insightspec-actor-identity-admin`

**Preconditions**:
- Identity resolution has run and produced pending match candidates
- User is authenticated with Identity Admin role

**Main Flow**:
1. Identity Admin queries the pending identity matches API, which returns pairs of records with confidence scores
2. Identity Admin reviews a pending match, comparing the two records' attributes (name, email, department, source systems)
3. Identity Admin confirms the match, instructing the Backend to merge the two records into a single golden record
4. Backend merges the records and updates all downstream references
5. Backend records the merge decision in the audit trail, including the administrator's identity and the merged record IDs

**Postconditions**: The two records are merged into one golden record. All analytics data referencing either original record now points to the merged record. Audit trail records the decision.

**Alternative Flows**:
- **Reject match**: If the administrator determines the records are different people, they reject the match. The Backend marks the pair as "rejected" so it is not proposed again.
- **Split existing record**: If the administrator discovers a golden record that incorrectly merged two people, they initiate a split operation, specifying which source identities belong to each resulting record.

### 8.4 Access Control Configuration

#### Granting Org-Scoped Access

- [ ] `p1` - **ID**: `cpt-insightspec-usecase-rbac-grant`

**Actor**: `cpt-insightspec-actor-tenant-admin`

**Preconditions**:
- User is authenticated with Tenant Admin role
- The target user exists in the OIDC provider
- The org tree has been synced from the HR system

**Main Flow**:
1. Tenant Admin searches for the target user by name or email
2. Tenant Admin assigns a role (Viewer, Analyst, Connector Admin, or Identity Admin) to the user
3. Tenant Admin browses the org tree and selects the org units the user should have visibility into
4. Backend creates the role assignment with the specified org-unit scope grants
5. Backend records the grant in the audit trail

**Postconditions**: The target user can now access data and features per their assigned role, scoped to the granted org units. Audit trail records who made the grant, to whom, and which org units were included.

**Alternative Flows**:
- **User not found**: If the user has not yet authenticated via OIDC, the Backend allows creating a pre-provisioned role assignment that activates upon the user's first login.
- **Conflicting grants**: If the user already has a role assignment, the Backend returns the existing assignment details and allows the administrator to modify it.

### 8.5 Alerting

#### Configuring a Metric Alert

- [ ] `p2` - **ID**: `cpt-insightspec-usecase-alert-setup`

**Actor**: `cpt-insightspec-actor-analyst`

**Preconditions**:
- User is authenticated with a role that permits alert creation (Analyst or Tenant Admin)
- User has org-unit scope grants for the data the alert will monitor

**Main Flow**:
1. Analyst selects a metric (e.g., "Average PR Review Time") and an org-unit scope
2. Analyst defines a threshold condition (e.g., "above 48 hours") and evaluation frequency (e.g., daily)
3. Analyst specifies notification recipients (email addresses)
4. Backend validates that the user has visibility into the specified org-unit scope
5. Backend creates the alert rule

**Postconditions**: Alert rule is active. The Backend will evaluate it at the configured frequency and send email notifications when the threshold is breached.

**Alternative Flows**:
- **Scope exceeds grants**: If step 4 finds the user is requesting alert scope beyond their granted org units, the Backend rejects the request with an authorization error.

### 8.6 Compliance Audit

#### Investigating Data Access for a Specific Employee

- [ ] `p1` - **ID**: `cpt-insightspec-usecase-audit-investigate`

**Actor**: `cpt-insightspec-actor-tenant-admin`

**Preconditions**:
- Tenant Admin is authenticated with Tenant Admin role
- Audit trail is operational

**Main Flow**:
1. Tenant Admin queries the audit API with filters: target resource = specific employee's golden record ID, time range = last 90 days
2. Backend returns all audit events matching the filter (analytics queries that included this person's data, any identity resolution actions on this person's record, any scope grants that included this person's org unit)
3. Tenant Admin reviews the events to verify all access was authorized and appropriate

**Postconditions**: Tenant Admin has a complete record of all system interactions involving the specified employee's data.

**Alternative Flows**:
- **No matching events**: If no audit events match the filter, the Backend returns an empty result set. This is a valid outcome (the employee's data was not accessed in the time range).

### 8.7 Platform Setup

#### Initial Platform Configuration

- [ ] `p1` - **ID**: `cpt-insightspec-usecase-platform-setup`

**Actor**: `cpt-insightspec-actor-tenant-admin`

**Preconditions**:
- Insight Backend is deployed on the customer's Kubernetes cluster
- OIDC provider is configured and reachable
- SMTP service is configured

**Main Flow**:
1. Tenant Admin authenticates via OIDC for the first time
2. Backend detects this is the first admin user and assigns the Tenant Administrator role
3. Tenant Admin configures the HR system connector to sync the org tree
4. Tenant Admin waits for the initial org tree sync to complete
5. Tenant Admin configures data source connectors (Git, task tracking, communication, etc.)
6. Tenant Admin defines role assignments and org-unit scope grants for other users
7. Tenant Admin configures initial alert rules for key metrics

**Postconditions**: Platform is fully configured with connectors syncing, RBAC policies defined, and alerts active. All configuration steps are recorded in the audit trail.

**Alternative Flows**:
- **HR sync fails**: If step 4 fails, the administrator can still proceed with connector configuration but cannot assign org-unit scopes until the org tree is available.

### 8.8 Transformation Monitoring

#### Diagnosing Stale Dashboard Data

- [ ] `p2` - **ID**: `cpt-insightspec-usecase-transform-diagnose`

**Actor**: `cpt-insightspec-actor-connector-admin`

**Preconditions**:
- An analyst reports that dashboard data appears stale
- User is authenticated with Connector Admin role or higher

**Main Flow**:
1. Connector Admin checks connector status API to verify data is arriving from source systems
2. Connectors show recent successful syncs, so the issue is not in ingestion
3. Connector Admin checks transformation execution status API
4. Backend shows that the latest transformation run failed with an error
5. Connector Admin reviews the error details and identifies the root cause
6. Connector Admin triggers a manual transformation re-run

**Postconditions**: Root cause identified. After the transformation re-runs successfully, dashboard data will be current.

**Alternative Flows**:
- **Connector is the issue**: If step 2 reveals a connector has not synced recently, the administrator investigates the connector error details instead.

## 9. Acceptance Criteria

- [ ] `cpt-insightspec-fr-be-analytics-read`, `cpt-insightspec-fr-be-visibility-policy`: Authenticated user can query analytics data scoped to their org unit and membership period. A user with scope grants for Department A **MUST NOT** see data for Department B employees in any API response.
- [ ] `cpt-insightspec-nfr-be-tenant-isolation`: Tenant A cannot access Tenant B data through any API endpoint. Zero cross-tenant data leaks verified via automated cross-tenant access tests.
- [ ] `cpt-insightspec-fr-be-connector-crud`, `cpt-insightspec-fr-be-secret-management`: A Connector Admin can complete the full connector onboarding flow (create, test connection, initial sync) through the API without direct Airbyte access.
- [ ] `cpt-insightspec-fr-be-business-alerts`, `cpt-insightspec-fr-be-email-delivery`: Business alert fires email within 10 minutes of threshold breach.
- [ ] `cpt-insightspec-fr-be-audit-trail`: Audit trail captures all data access and configuration changes with queryable retention. Audit records are immutable -- no API or internal function can modify or delete an audit event after creation.
- [ ] `cpt-insightspec-fr-be-identity-resolution-service`: Identity resolution maps aliases from multiple sources into a single person golden record. Merge and split operations propagate to all downstream analytics data within one transformation cycle.
- [ ] `cpt-insightspec-fr-be-transform-rules`: Transformation rules can be configured and triggered, producing unified and metric tables with observable status.
- [ ] `cpt-insightspec-fr-be-forward-only-migrations`: Database migrations execute automatically during deployment with zero-downtime rolling deployments.
- [ ] `cpt-insightspec-fr-be-health-check`: Health check endpoint accurately reports the status of all critical dependencies and distinguishes liveness from readiness.
- [ ] `cpt-insightspec-fr-be-oidc-auth`, `cpt-insightspec-fr-be-rbac`: All API requests are authenticated via OIDC and authorized via RBAC. Unauthenticated or unauthorized requests are rejected with appropriate HTTP status codes.
- [ ] `cpt-insightspec-fr-be-audit-trail`: All configuration changes (connector CRUD, RBAC grants, alert rules) produce audit trail entries with complete actor and action details.
- [ ] `cpt-insightspec-nfr-be-retry-resilience`: System recovers from dependency failures (ClickHouse, MariaDB, LDAP) within retry budget without data loss.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| ClickHouse | Analytics storage (Silver/Gold layers) and audit log | `p1` |
| MariaDB | Per-service metadata storage (configs, secrets, org tree, alerts, email) | `p1` |
| Redis | Caching and rate limiting | `p2` |
| Redpanda | Event streaming for audit events, email requests, cache invalidation | `p1` |
| MinIO | S3-compatible storage for CSV exports | `p2` |
| Airbyte | Data extraction platform (connector management via API) | `p1` |
| Kestra | Pipeline orchestration -- scheduling, retries, transformation runs (used by Connector Manager and Transform Service) | `p1` |
| Customer OIDC provider | Authentication | `p1` |
| Customer HR/directory system | Organizational hierarchy source (AD, BambooHR, Workday, etc.) | `p1` |
| Customer SMTP server | Email delivery | `p2` |

## 11. Assumptions

- Customer has an OIDC-compliant identity provider capable of issuing JWT tokens
- Customer has an HR or directory system that provides organizational hierarchy (Active Directory, BambooHR, Workday, or similar)
- Customer provides an SMTP server for outbound email
- Kubernetes cluster has sufficient resources for all bundled infrastructure (ClickHouse, MariaDB, Redis, Redpanda, MinIO, Airbyte, Kestra, monitoring stack)
- Airbyte API is stable enough for programmatic connection management
- Single MariaDB instance is sufficient for metadata workloads across all services

## 12. Open Questions

### OQ-BE-1: Bronze Write API ownership

Who is responsible for accepting RECORD messages from connectors and writing them to Bronze tables? The Connector Framework (ADR-0002) defines thin extractors that produce stdout JSON, but it is unclear whether the Backend exposes a Bronze Write API or whether this is entirely handled by the Ingestion/Orchestrator layer. If the Backend owns writes, it must handle batch semantics, deduplication, and tenant_id enforcement at the API level.

### OQ-BE-2: Collection runs tracking

`CONNECTORS_REFERENCE.md` defines `{source}_collection_runs` tables for sync monitoring. The Backend PRD mentions connector status (§5.2) but does not specify who creates and populates these tracking tables. Is this an Ingestion concern, or does the Backend maintain them?

### OQ-BE-3: Permission model — ScopeGrants and SourceAccess

The project's Permission Architecture (`PERMISSION_PRD.md`) defines a three-tier model: RBAC + Org-Hierarchy Scoping + **ScopeGrants** (time-bounded cross-hierarchy overrides for functional specialists such as HR or auditors). Additionally, **SourceAccess** controls restrict certain data domains (e.g., Allure, HubSpot) by role. The current Backend PRD covers only RBAC + Org Scoping (§5.3). Should ScopeGrants and SourceAccess be added as explicit FRs, or are they deferred to a later phase?

### OQ-BE-4: Identity Resolution coordination

The Identity Resolution domain (`docs/domain/identity-resolution/`) defines a Bootstrap Job that seeds the identity store from `class_people` Silver tables and a Resolution Service that produces Silver step 2 (identity-resolved) tables. The Backend PRD (§5.4) describes golden record management but does not specify:
- Who triggers the Bootstrap Job after `class_people` is available?
- Does the Backend expose a person_id lookup API for analytics queries?
- How is the Silver step 1 → identity resolution → Silver step 2 pipeline coordinated?

### OQ-BE-5: API Guidelines conformance

The project defines shared API conventions in `docs/shared/api-guideline/` (REST principles, pagination, filtering syntax, batch operations, error format per RFC 9457). Should the Backend PRD explicitly reference these as normative, or is this assumed via the DESIGN document?

### OQ-BE-6: Incremental sync state persistence

Connectors emit STATE messages with cursors for incremental synchronization. Someone must persist these cursors so that syncs can resume after failure. Is this the Backend's responsibility, or does Airbyte/Orchestrator handle state persistence entirely?

### OQ-BE-7: Schema validation at ingestion boundary

`CONNECTORS_REFERENCE.md` is the source of truth for Bronze and Silver schemas. Should the Backend validate incoming data against expected schemas at write time, or is schema enforcement delegated to the transformation layer (dbt)?

### OQ-BE-8: Orchestrator migration — Kestra vs Argo Workflows

PR #45 (`feat/ingestion: migrate to Kind K8s + Argo Workflows`) proposes replacing Kestra with Argo Workflows as the pipeline orchestrator, with a new ADR-0002 (`0002-argo-over-kestra.md`). If merged, the Kestra API Contract (§7.2 `cpt-insightspec-contract-kestra`) and the Kestra dependency (§10) become obsolete. The Backend PRD should either abstract the orchestrator reference or be updated after the migration decision is finalized.

## 13. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Airbyte API breaking changes | Connector Manager integration breaks on Airbyte upgrades | Abstract Airbyte API behind adapter layer; pin Airbyte version in Helm chart |
| ClickHouse single-node capacity limits | Query performance degrades with large data volumes | Vertical scaling first; sharding architecture designed but deferred to v2 |
| Org source sync latency | Org tree updates delayed; stale access scopes | Configurable sync interval; manual sync trigger for Identity Admins; cache TTL limits stale window |
| Per-tenant encryption key management complexity | Key rotation errors could lock out tenant | Automated key rotation tested in integration suite; master key rotation only re-wraps tenant keys |
| Redpanda-to-Kafka migration | Future migration may introduce compatibility issues | Use only Kafka-compatible rdkafka API; no Redpanda-specific features |
| Customer K8s cluster variability | Helm chart may not work on all K8s distributions | Test on EKS, GKE, AKS, and k3s; document minimum resource requirements |
| Identity resolution ambiguity | Same person may have conflicting aliases across sources; false merges corrupt analytics | Conflict detection with manual override; merge/split audit trail; conservative matching defaults |
| Transformation failures | Broken transform rules block unified/metric pipeline | Transform status monitoring via event stream; alerts on failure; transforms are idempotent and re-runnable |
| Kestra API breaking changes | Transform Service integration breaks on Kestra upgrades | Abstract Kestra API behind adapter layer; pin Kestra version in Helm chart |
