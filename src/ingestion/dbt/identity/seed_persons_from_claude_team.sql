-- Phase 1 (Initial Seed): Claude Team users → person.persons
-- Idempotent: skips users whose email already exists in persons.
-- Dedup: takes latest row per email by _airbyte_extracted_at.
-- Source: docs/domain/identity-resolution/specs/DECOMPOSITION.md §2.1
--
-- Prerequisite: person.persons table created by scripts/migrations/20260408000000_init-identity.sql
-- Run: dbt run --select seed_persons_from_claude_team
--
-- Test manually: http://localhost:30123/play  (user: default, password: clickhouse_local)
--   or:          http://localhost:8123/play

{{ config(
    materialized='incremental',
    unique_key='id',
    schema='person',
    tags=['identity:seed', 'person']
) }}

WITH latest AS (
    SELECT email, name, role, type, tenant_id
    FROM {{ source('bronze_claude_team', 'claude_team_users') }}
    WHERE email IS NOT NULL AND email != ''
    QUALIFY row_number() OVER (PARTITION BY email ORDER BY _airbyte_extracted_at DESC) = 1
)

SELECT
    generateUUIDv7()                                        AS id,
    UUIDNumToString(sipHash128(coalesce(tenant_id, '')))             AS insight_tenant_id,
    coalesce(name, '')                                      AS display_name,
    'claude_team'                                           AS display_name_source,
    'active'                                                AS status,
    lower(trim(email))                                      AS email,
    'claude_team'                                           AS email_source,
    ''                                                      AS username,
    ''                                                      AS username_source,
    coalesce(role, '')                                      AS role,
    'claude_team'                                           AS role_source,
    toUUID('00000000-0000-0000-0000-000000000000')          AS manager_person_id,
    ''                                                      AS manager_person_id_source,
    toUUID('00000000-0000-0000-0000-000000000000')          AS org_unit_id,
    ''                                                      AS org_unit_id_source,
    ''                                                      AS location,
    ''                                                      AS location_source,
    (if(name IS NOT NULL AND name != '', 1, 0)
     + if(email != '', 1, 0)
     + if(role IS NOT NULL AND role != '', 1, 0)
    ) / 7.0                                                 AS completeness_score,
    'clean'                                                 AS conflict_status,
    now64(3)                                                AS created_at,
    now64(3)                                                AS updated_at,
    0                                                       AS is_deleted
FROM latest l
WHERE lower(trim(l.email)) NOT IN (
    SELECT lower(email) FROM person.persons WHERE is_deleted = 0
)
