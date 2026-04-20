"""Bitbucket Cloud branches stream (incremental by target.date, HttpSubStream of repositories)."""

import logging
from typing import Any, Iterable, Mapping, MutableMapping, Optional

from airbyte_cdk.models import SyncMode
from airbyte_cdk.sources.streams.http import HttpSubStream

from source_bitbucket_cloud.streams.base import BitbucketCloudStream, _make_unique_key

logger = logging.getLogger("airbyte")


class BranchesStream(HttpSubStream, BitbucketCloudStream):
    """Branches for each repository.

    Incremental per-repo cursor on ``target_date`` (latest commit date on branch).
    Having cursor_field skips the CDK auto-assigned ResumableFullRefreshCursor so
    the commits stream can re-iterate branches as a parent via HttpSubStream.

    Deleted branches are not re-emitted (no API signal). This is acceptable: bronze
    is a data lake, keeping historical branch rows is desirable.
    """

    name = "branches"
    cursor_field = "target_date"
    use_cache = True
    state_checkpoint_interval = 500

    def __init__(self, parent, **kwargs: Any) -> None:
        super().__init__(parent=parent, **kwargs)
        self._stop_pagination: bool = False

    def _path(self, stream_slice: Optional[Mapping[str, Any]] = None) -> str:
        s = stream_slice or {}
        repo = s["parent"]
        return f"repositories/{repo['workspace']}/{repo['slug']}/refs/branches"

    def stream_slices(
        self,
        sync_mode: SyncMode,
        cursor_field: Optional[list] = None,
        stream_state: Optional[Mapping[str, Any]] = None,
    ) -> Iterable[Optional[Mapping[str, Any]]]:
        state = stream_state or {}
        slice_count = 0
        for parent_slice in super().stream_slices(
            sync_mode=sync_mode, cursor_field=cursor_field, stream_state=stream_state,
        ):
            repo = (parent_slice or {}).get("parent") or {}
            workspace = repo.get("workspace")
            slug = repo.get("slug")
            if not workspace or not slug:
                continue
            partition_key = f"{workspace}/{slug}"
            cursor = (state.get(partition_key, {}) or {}).get(self.cursor_field, "") or ""
            self._stop_pagination = False
            slice_count += 1
            yield {
                "parent": repo,
                "cursor_value": cursor,
                "partition_key": partition_key,
            }
        logger.info(f"branches: iterated {slice_count} repo slices")

    def request_params(
        self,
        stream_state: Optional[Mapping[str, Any]] = None,
        stream_slice: Optional[Mapping[str, Any]] = None,
        next_page_token: Optional[Mapping[str, Any]] = None,
    ) -> Mapping[str, Any]:
        if next_page_token:
            return {}
        params: dict[str, Any] = {
            "pagelen": str(self.page_size),
            "sort": "-target.date",
        }
        cursor = (stream_slice or {}).get("cursor_value", "") or ""
        if cursor:
            params["q"] = f'target.date>"{cursor}"'
        return params

    def next_page_token(self, response):
        if self._stop_pagination:
            self._stop_pagination = False
            return None
        return super().next_page_token(response)

    def parse_response(
        self,
        response,
        stream_slice: Optional[Mapping[str, Any]] = None,
        **kwargs: Any,
    ):
        s = stream_slice or {}
        repo = s["parent"]
        workspace = repo["workspace"]
        slug = repo["slug"]
        default_branch_name = repo.get("mainbranch_name", "")
        repo_updated_on = repo.get("updated_on", "")
        cursor_value = s.get("cursor_value", "") or ""
        emitted = 0

        for branch in self._iter_values(response):
            branch_name = branch.get("name", "")
            target = branch.get("target") or {}
            target_hash = target.get("hash", "")
            target_date = target.get("date", "") or ""

            if cursor_value and target_date and target_date <= cursor_value:
                self._stop_pagination = True
                logger.info(
                    f"branches: {workspace}/{slug} cursor early-exit at "
                    f"target_date={target_date} cursor={cursor_value}"
                )
                return
            emitted += 1

            record = {
                "unique_key": _make_unique_key(
                    self._tenant_id, self._source_id, workspace, slug, branch_name,
                ),
                "name": branch_name,
                "target": target,
                "target_hash": target_hash,
                "target_date": target_date,
                "workspace": workspace,
                "repo_slug": slug,
                "mainbranch_name": default_branch_name,
                "default_branch_name": default_branch_name,
                "is_default": branch_name == default_branch_name,
                "updated_on": repo_updated_on,
            }
            yield self._envelope(record)

        logger.debug(
            f"branches: repo={workspace}/{slug} page_emitted={emitted}"
        )

    def get_updated_state(
        self,
        current_stream_state: MutableMapping[str, Any],
        latest_record: Mapping[str, Any],
    ) -> MutableMapping[str, Any]:
        workspace = latest_record.get("workspace", "")
        slug = latest_record.get("repo_slug", "")
        if not workspace or not slug:
            return current_stream_state
        partition_key = f"{workspace}/{slug}"
        target_date = latest_record.get(self.cursor_field, "") or ""
        if not target_date:
            return current_stream_state
        entry = dict(current_stream_state.get(partition_key, {}) or {})
        prev = entry.get(self.cursor_field, "") or ""
        if target_date > prev:
            entry[self.cursor_field] = target_date
            current_stream_state[partition_key] = entry
        return current_stream_state

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
                "target_hash": {"type": ["null", "string"]},
                "target_date": {"type": ["null", "string"]},
                "workspace": {"type": "string"},
                "repo_slug": {"type": "string"},
                "mainbranch_name": {"type": ["null", "string"]},
                "default_branch_name": {"type": ["null", "string"]},
                "is_default": {"type": ["null", "boolean"]},
                "updated_on": {"type": ["null", "string"]},
            },
        }
