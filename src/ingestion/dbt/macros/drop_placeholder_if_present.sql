{# ---------------------------------------------------------------------------
   drop_placeholder_if_present(relation)
   ---------------------------------------------------------------------------
   Project-level pre_hook for silver dbt models. Drops the placeholder
   table created by `scripts/create-bronze-placeholders.sh` so the dbt
   incremental materialization rebuilds it with the model's real schema
   (engine, ORDER BY, full column list) on the first run.

   Without this hook, dbt-clickhouse's `materialized='incremental'`
   detects the placeholder as an existing relation and `INSERT INTO`s the
   placeholder schema — leaving the placeholder's minimum-viable shape in
   place and silently dropping any columns the staging model emits that
   the placeholder does not have (default `on_schema_change='ignore'`).
   The result was schema drift between the placeholder and the real
   silver model with no static check; only live test catches it.

   Detection strategy: two-factor signature, both must hold.

     1. system.tables.comment matches the literal marker
        `INSIGHT_PLACEHOLDER_v1` set by create-bronze-placeholders.sh on
        every silver placeholder it creates.
     2. system.tables.total_rows == 0. A real dbt-managed silver table
        has rows after its first incremental run; a placeholder is
        always empty (init.sh creates it with no INSERT). This guards
        against the edge case where someone manually ran `ALTER TABLE
        ... MODIFY COMMENT` on a real table to the placeholder marker.

   Both checks together make accidental DROP of a real table
   essentially impossible — drop is gated on (PLACEHOLDER marker AND no
   data), so a wrongly-marked-but-populated table is preserved.

   Removal plan: when gold-view migrations are split into a post-dbt
   phase (Variant A in ADR-0007), silver tables will only be created by
   dbt itself — placeholders disappear, this macro becomes dead code,
   and both this macro and the COMMENT clauses in
   create-bronze-placeholders.sh can be deleted.
   --------------------------------------------------------------------------- #}
{% macro drop_placeholder_if_present(relation) %}
    {% if not execute %}
        {# parse phase — skip the runtime query #}
        {% do return(none) %}
    {% endif %}

    {% set check_query %}
        SELECT count() AS n
        FROM system.tables
        WHERE database   = '{{ relation.schema }}'
          AND name       = '{{ relation.identifier }}'
          AND comment    = 'INSIGHT_PLACEHOLDER_v1'
          AND total_rows = 0
    {% endset %}

    {% set result = run_query(check_query) %}
    {% if result and result.rows and (result.rows[0][0] | int) > 0 %}
        {% do log("drop_placeholder_if_present: dropping " ~ relation ~ " (placeholder marker + 0 rows)", info=True) %}
        {% do run_query("DROP TABLE IF EXISTS " ~ relation) %}
    {% endif %}
{% endmacro %}
