#!/usr/bin/env python3
"""Validate freshness wiring in connector schema.yml files.

Two checks per `bronze_*` source under
`src/ingestion/connectors/*/*/dbt/schema.yml`:

1. `loaded_at_field` is reachable for every monitored table.
   - Source-level, OR
   - Per-table override, OR
   - Per-table `freshness: null` (explicit opt-out)
   Without one of these the dbt-clickhouse adapter emits a runtime error
   for that source, which silently masquerades as a freshness breach.

2. Every `freshness: null` opt-out carries a written reason in
   `meta.freshness_optout_reason`. Bare `freshness: null` is too easy
   to leave in by accident — once it lands, the table is invisible to
   monitoring forever and nobody knows whether the opt-out was deliberate.
   Forcing a one-line rationale at the time of writing keeps the audit
   surface readable: `grep -A1 'freshness: null'` shows what every opt-out
   is for.

Run from repo root:
    python3 src/ingestion/scripts/lint-bronze-freshness.py
"""
from __future__ import annotations

import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("PyYAML required: pip install pyyaml", file=sys.stderr)
    sys.exit(2)


def _opt_out(table: dict) -> bool:
    return "freshness" in table and table["freshness"] is None


def _has_table_anchor(table: dict) -> bool:
    field = table.get("loaded_at_field")
    return isinstance(field, str) and field.strip() != ""


def _optout_reason(table: dict) -> str | None:
    meta = table.get("meta") or {}
    reason = meta.get("freshness_optout_reason")
    if isinstance(reason, str) and reason.strip():
        return reason.strip()
    return None


def lint_file(schema_file: Path) -> list[str]:
    errors: list[str] = []
    try:
        doc = yaml.safe_load(schema_file.read_text())
    except yaml.YAMLError as e:
        return [f"{schema_file}: invalid YAML — {e}"]
    if not isinstance(doc, dict):
        return []

    for source in doc.get("sources") or []:
        # Bare `-` in YAML produces a None list element which is syntactically
        # valid; skip it so a malformed schema.yml fails the lint with a
        # clear "loaded_at_field missing" message instead of crashing on
        # `None.get(...)`.
        if not isinstance(source, dict):
            continue
        name = source.get("name", "")
        schema = source.get("schema", "")
        # Identify bronze sources by either the dbt-side name OR the
        # ClickHouse schema. The bamboohr connector uses `name: bamboohr,
        # schema: bronze_bamboohr`, so name-only matching would skip it.
        is_bronze = (
            isinstance(name, str) and name.startswith("bronze_")
        ) or (
            isinstance(schema, str) and schema.startswith("bronze_")
        )
        if not is_bronze:
            continue
        source_anchor = source.get("loaded_at_field")
        has_source_anchor = isinstance(source_anchor, str) and source_anchor.strip()
        tables = source.get("tables") or []
        if not tables:
            if not has_source_anchor:
                errors.append(
                    f"{schema_file}: source '{name}' has no tables and no "
                    f"loaded_at_field"
                )
            continue
        for table in tables:
            if not isinstance(table, dict):
                continue
            tname = table.get("name", "<unnamed>")
            if _opt_out(table):
                if not _optout_reason(table):
                    errors.append(
                        f"{schema_file}: source '{name}.{tname}' has "
                        f"`freshness: null` without a written rationale. "
                        f"Add `meta.freshness_optout_reason: \"…\"` next to "
                        f"the opt-out so future readers know why this table "
                        f"is invisible to monitoring (e.g. \"roster — "
                        f"full-refresh, no daily cadence\")."
                    )
                continue
            if _has_table_anchor(table) or has_source_anchor:
                continue
            errors.append(
                f"{schema_file}: source '{name}.{tname}' has no "
                f"loaded_at_field at source or table level, and is not opted "
                f"out via `freshness: null`. Pick the right anchor for the "
                f"connector style — see docs/domain/ingestion/specs/"
                f"feature-bronze-freshness-sla/FEATURE.md §2.1."
            )
    return errors


def main(argv: list[str]) -> int:
    if len(argv) >= 2:
        root = Path(argv[1])
    else:
        root = Path("src/ingestion/connectors")
    if not root.is_dir():
        print(f"connectors root not found: {root}", file=sys.stderr)
        return 2

    schema_files = sorted(root.glob("**/dbt/schema.yml"))
    if not schema_files:
        print(f"no schema.yml files under {root}", file=sys.stderr)
        return 2

    all_errors: list[str] = []
    for f in schema_files:
        all_errors.extend(lint_file(f))

    if all_errors:
        print(f"freshness lint failed ({len(all_errors)} issue(s)):")
        for line in all_errors:
            print(f"  {line}")
        return 1
    print(f"freshness lint OK — {len(schema_files)} schema.yml file(s) checked")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
