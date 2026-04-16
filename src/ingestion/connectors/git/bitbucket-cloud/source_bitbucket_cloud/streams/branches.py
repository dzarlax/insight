"""Bitbucket Cloud branches stream (REST, full refresh, child of repositories)."""

import json
import logging
import os
import tempfile
from typing import Any, Iterable, Mapping, Optional

from source_bitbucket_cloud.streams.base import BitbucketCloudRestStream, _make_unique_key
from source_bitbucket_cloud.streams.repositories import RepositoriesStream

logger = logging.getLogger("airbyte")


class BranchesStream(BitbucketCloudRestStream):
    """Fetches branches for each repository."""

    name = "branches"
    use_cache = True

    def __init__(self, parent: RepositoriesStream, **kwargs):
        super().__init__(**kwargs)
        self._parent = parent
        self._child_records_file = tempfile.NamedTemporaryFile(
            mode="w", prefix="insight_bb_branches_", suffix=".jsonl", delete=False,
        )
        self._child_records_path = self._child_records_file.name

    def _path(self, stream_slice: Optional[Mapping[str, Any]] = None, **kwargs) -> str:
        s = stream_slice or {}
        workspace = s.get("workspace", "")
        slug = s.get("slug", "")
        if not workspace or not slug:
            raise ValueError("BranchesStream._path() called without workspace/slug in stream_slice")
        return f"repositories/{workspace}/{slug}/refs/branches"

    def stream_slices(self, **kwargs) -> Iterable[Optional[Mapping[str, Any]]]:
        for record in self._parent.get_child_records():
            workspace = record.get("workspace", "")
            slug = record.get("slug", "")
            mainbranch_name = record.get("mainbranch_name", "")
            updated_on = record.get("updated_on", "")
            if workspace and slug:
                yield {
                    "workspace": workspace,
                    "slug": slug,
                    "mainbranch_name": mainbranch_name,
                    "updated_on": updated_on,
                }

    def parse_response(self, response, stream_slice=None, **kwargs):
        s = stream_slice or {}
        workspace = s.get("workspace", "")
        slug = s.get("slug", "")
        if not self._guard_response(response):
            return
        data = response.json()
        branches = data.get("values", [])
        for branch in branches:
            branch_name = branch.get("name", "")
            target = branch.get("target") or {}
            target_hash = target.get("hash", "")

            branch["unique_key"] = _make_unique_key(
                self._tenant_id, self._source_id, workspace, slug, branch_name,
            )
            branch["workspace"] = workspace
            branch["repo_slug"] = slug
            branch["mainbranch_name"] = s.get("mainbranch_name", "")
            branch["updated_on"] = s.get("updated_on", "")

            # Write minimal child data to disk for commits stream
            self._child_records_file.write(json.dumps({
                "name": branch_name,
                "workspace": workspace,
                "repo_slug": slug,
                "mainbranch_name": s.get("mainbranch_name", ""),
                "updated_on": s.get("updated_on", ""),
                "target_hash": target_hash,
            }, separators=(",", ":")) + "\n")
            yield self._add_envelope(branch)

    def get_child_records(self) -> Iterable:
        """Yield branch records from disk. Zero memory, zero API calls."""
        if self._child_records_file and not self._child_records_file.closed:
            self._child_records_file.close()
        if not os.path.exists(self._child_records_path):
            return
        with open(self._child_records_path, "r") as f:
            for line in f:
                line = line.rstrip("\n")
                if line:
                    yield json.loads(line)

    def get_json_schema(self) -> Mapping[str, Any]:
        return {
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "additionalProperties": True,
            "properties": {
                "tenant_id": {"type": "string"},
                "source_id": {"type": "string"},
                "unique_key": {"type": "string"},
                "data_source": {"type": "string"},
                "collected_at": {"type": "string"},
                "name": {"type": ["null", "string"]},
                "target": {"type": ["null", "object"]},
                "workspace": {"type": "string"},
                "repo_slug": {"type": "string"},
                "mainbranch_name": {"type": ["null", "string"]},
                "updated_on": {"type": ["null", "string"]},
            },
        }
