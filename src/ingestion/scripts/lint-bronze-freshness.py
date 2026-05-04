#!/usr/bin/env python3
"""Validate freshness wiring in connector schema.yml files.

Every `bronze_*` source declared under `src/ingestion/connectors/*/*/dbt/schema.yml`
must have `loaded_at_field` so that `dbt source freshness` can compute a max
timestamp. Without it the dbt-clickhouse adapter emits a runtime error per
row, which silently masquerades as a freshness breach.

A source can satisfy the rule three ways:
  1. Source-level `loaded_at_field` (covers all tables).
  2. Per-table `loaded_at_field` (override for that table only).
  3. Per-table `freshness: null` (explicit opt-out — catalog/roster streams).

Mixed forms are allowed. The rule fails only when at least one table has
neither own `loaded_at_field` nor `freshness: null` AND the source has no
`loaded_at_field` either.

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


def lint_file(schema_file: Path) -> list[str]:
    errors: list[str] = []
    try:
        doc = yaml.safe_load(schema_file.read_text())
    except yaml.YAMLError as e:
        return [f"{schema_file}: invalid YAML — {e}"]
    if not isinstance(doc, dict):
        return []

    for source in doc.get("sources") or []:
        name = source.get("name", "")
        if not isinstance(name, str) or not name.startswith("bronze_"):
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
        if has_source_anchor:
            continue
        for table in tables:
            tname = table.get("name", "<unnamed>")
            if _opt_out(table) or _has_table_anchor(table):
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
