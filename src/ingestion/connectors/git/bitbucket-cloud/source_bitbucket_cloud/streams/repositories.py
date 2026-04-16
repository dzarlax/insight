"""Bitbucket Cloud repositories stream (REST, full refresh)."""

import json
import logging
import os
import tempfile
from typing import Any, Iterable, Mapping, Optional

from source_bitbucket_cloud.streams.base import BitbucketCloudRestStream, _make_unique_key

logger = logging.getLogger("airbyte")


class RepositoriesStream(BitbucketCloudRestStream):
    """Fetches all repositories for configured workspaces via REST API."""

    name = "repositories"
    use_cache = True

    def __init__(
        self,
        workspaces: list[str],
        skip_forks: bool = True,
        **kwargs,
    ):
        super().__init__(**kwargs)
        self._workspaces = workspaces
        self._skip_forks = skip_forks
        self._child_records_file = tempfile.NamedTemporaryFile(
            mode="w", prefix="insight_bb_repos_", suffix=".jsonl", delete=False,
        )
        self._child_records_path = self._child_records_file.name

    def _path(self, stream_slice: Optional[Mapping[str, Any]] = None, **kwargs) -> str:
        workspace = (stream_slice or {}).get("workspace", "")
        if not workspace:
            raise ValueError("RepositoriesStream._path() called without workspace in stream_slice")
        return f"repositories/{workspace}"

    def request_params(self, **kwargs) -> dict:
        return {"pagelen": "100"}

    def stream_slices(self, **kwargs) -> Iterable[Optional[Mapping[str, Any]]]:
        for workspace in self._workspaces:
            yield {"workspace": workspace}

    def parse_response(self, response, stream_slice=None, **kwargs):
        workspace = (stream_slice or {}).get("workspace", "")
        if not self._guard_response(response):
            return
        data = response.json()
        repos = data.get("values", [])
        skipped = 0
        for repo in repos:
            if self._skip_forks and repo.get("parent"):
                skipped += 1
                continue

            slug = repo.get("slug", "")
            repo_name = repo.get("name", "")
            mainbranch = (repo.get("mainbranch") or {}).get("name", "")
            updated_on = repo.get("updated_on", "")
            project = repo.get("project") or {}

            repo["unique_key"] = _make_unique_key(
                self._tenant_id, self._source_id, workspace, slug,
            )
            repo["workspace"] = workspace
            repo["project_key"] = project.get("key")
            repo["project_name"] = project.get("name")
            record = self._add_envelope(repo)

            # Write minimal child data to disk for child streams
            self._child_records_file.write(json.dumps({
                "workspace": workspace,
                "slug": slug,
                "name": repo_name,
                "updated_on": updated_on,
                "mainbranch_name": mainbranch,
            }, separators=(",", ":")) + "\n")
            yield record
        if skipped:
            logger.info(f"Repo filter: skipped {skipped} forked repos in workspace {workspace}")

    def get_child_records(self) -> Iterable:
        """Yield repo records from disk. Zero memory, zero API calls."""
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
                "workspace": {"type": "string"},
                "uuid": {"type": ["null", "string"]},
                "slug": {"type": ["null", "string"]},
                "name": {"type": ["null", "string"]},
                "full_name": {"type": ["null", "string"]},
                "is_private": {"type": ["null", "boolean"]},
                "description": {"type": ["null", "string"]},
                "language": {"type": ["null", "string"]},
                "created_on": {"type": ["null", "string"]},
                "updated_on": {"type": ["null", "string"]},
                "size": {"type": ["null", "integer"]},
                "has_issues": {"type": ["null", "boolean"]},
                "has_wiki": {"type": ["null", "boolean"]},
                "fork_policy": {"type": ["null", "string"]},
                "mainbranch": {"type": ["null", "object"]},
                "project_key": {"type": ["null", "string"]},
                "project_name": {"type": ["null", "string"]},
            },
        }
