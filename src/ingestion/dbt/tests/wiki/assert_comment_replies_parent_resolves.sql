-- AC#4 from issue #285: every parent_comment_id in a reply stream must
-- either be NULL (impossible by construction — these tables only get
-- populated by the /children endpoint, which always sets it) OR resolve
-- to an existing comment_id in the corresponding top-level stream.
--
-- The test fails (returns rows) when a footer/inline reply has a
-- parent_comment_id that does not appear in the corresponding top-level
-- comments table. Common causes:
--   - Top-level parent was deleted between the parent-stream sync and
--     the reply-stream sync (transient — should self-heal next run)
--   - Parent stream was filtered (resolved? archived?) and the reply
--     stream wasn't, leaving orphans
--   - A bug in SubstreamPartitionRouter wiring causing replies under
--     different kinds to be cross-attributed (footer reply pointing
--     at an inline-comment id, etc.)
--
-- The test is a sanity check; not a strict invariant — Confluence does
-- expose the comment-deleted state, and a deleted parent with surviving
-- replies is theoretically possible. If observed in production, debug
-- against the live API; if confirmed legitimate, add a tolerance window
-- (e.g. allow orphan replies created within last 24h).

SELECT
    'footer_reply' AS kind,
    r.unique_key,
    r.comment_id AS reply_comment_id,
    r.parent_comment_id,
    r.tenant_id,
    r.source_id
FROM {{ source('bronze_confluence', 'wiki_footer_comment_replies') }} r
LEFT JOIN {{ source('bronze_confluence', 'wiki_footer_comments') }} p
    ON r.tenant_id = p.tenant_id
   AND r.source_id = p.source_id
   AND r.parent_comment_id = p.comment_id
WHERE r.parent_comment_id IS NOT NULL
  AND r.parent_comment_id != ''
  AND p.comment_id IS NULL

UNION ALL

SELECT
    'inline_reply' AS kind,
    r.unique_key,
    r.comment_id AS reply_comment_id,
    r.parent_comment_id,
    r.tenant_id,
    r.source_id
FROM {{ source('bronze_confluence', 'wiki_inline_comment_replies') }} r
LEFT JOIN {{ source('bronze_confluence', 'wiki_inline_comments') }} p
    ON r.tenant_id = p.tenant_id
   AND r.source_id = p.source_id
   AND r.parent_comment_id = p.comment_id
WHERE r.parent_comment_id IS NOT NULL
  AND r.parent_comment_id != ''
  AND p.comment_id IS NULL

LIMIT 100
