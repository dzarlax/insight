-- MariaDB: persons table -- field-level identity attribute history.
-- Stores SCD-like observations: each row = one field value for a person at a point in time.
-- Populated by one-time seed from identity_inputs (ClickHouse view),
-- then maintained by identity resolution pipeline.
--
-- Idempotency: the unique index uq_person_observation guarantees that
-- re-running the seed with the same input produces no duplicates -- an
-- observation is fully identified by
-- (insight_tenant_id, person_id, insight_source_type, insight_source_id,
--  alias_type, alias_value). See ADR-0002 (deterministic person_id).
--
-- Column character sets (CodeRabbit review, PR #214):
--   UUID columns (CHAR(36)) and enum-like short strings use CHARACTER SET
--   ascii -- UUIDs are hex + dashes, source-type / alias-type values come
--   from a small ASCII enum. Keeps the UNIQUE KEY byte footprint under
--   InnoDB's 3072-byte prefix limit on non-DYNAMIC row formats and skips
--   ICU collation work on hot lookup paths.
--
--   `alias_value` keeps utf8mb4 (real names / emails / etc.) but uses
--   utf8mb4_bin so case/diacritics-differing observations count as
--   DISTINCT rows at the UNIQUE key level. Without _bin, "Foo@x.com"
--   and "foo@x.com" would collide and INSERT IGNORE would silently drop
--   one of them, corrupting the natural observation key.
--
-- Database: identity (dedicated to identity-resolution-domain tables;
-- analytics-api owns its own `analytics` database on the same MariaDB
-- instance -- see ADR-0005).
-- Source: docs/domain/identity-resolution/specs/DESIGN.md

CREATE TABLE IF NOT EXISTS persons (
    id                  BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    alias_type          VARCHAR(50)   CHARACTER SET ascii COLLATE ascii_general_ci NOT NULL COMMENT 'Field kind: email, display_name, platform_id, employee_id, etc.',
    insight_source_type VARCHAR(100)  CHARACTER SET ascii COLLATE ascii_general_ci NOT NULL COMMENT 'Source system: bamboohr, zoom, cursor, claude_admin, etc.',
    insight_source_id   CHAR(36)      CHARACTER SET ascii COLLATE ascii_general_ci NOT NULL COMMENT 'Connector instance UUID (sipHash from bronze source_id)',
    insight_tenant_id   CHAR(36)      CHARACTER SET ascii COLLATE ascii_general_ci NOT NULL COMMENT 'Tenant UUID (sipHash from bronze tenant_id)',
    alias_value         VARCHAR(512)  CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL COMMENT 'Field value (email address, display name, platform ID, etc.)',
    person_id           CHAR(36)      CHARACTER SET ascii COLLATE ascii_general_ci NOT NULL COMMENT 'Person UUID -- deterministic UUIDv5 from (insight_tenant_id, lower(trim(email)))',
    author_person_id    CHAR(36)      CHARACTER SET ascii COLLATE ascii_general_ci NOT NULL COMMENT 'Person UUID of who/what made this change',
    reason              TEXT          NOT NULL DEFAULT '' COMMENT 'Optional change reason / comment',
    created_at          DATETIME(3)   NOT NULL DEFAULT CURRENT_TIMESTAMP(3) COMMENT 'When this record was created',

    UNIQUE KEY uq_person_observation (
        insight_tenant_id, person_id, insight_source_type, insight_source_id,
        alias_type, alias_value
    ),
    INDEX idx_person_id (person_id),
    INDEX idx_tenant_person (insight_tenant_id, person_id),
    INDEX idx_alias_lookup (insight_tenant_id, alias_type, alias_value),
    INDEX idx_source (insight_source_type, insight_source_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
