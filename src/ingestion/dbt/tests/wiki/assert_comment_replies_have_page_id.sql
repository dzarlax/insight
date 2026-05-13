-- mitasovr review item (Major) on PR #358: reply rows in
-- wiki_*_comment_replies extract page_id via record.get('pageId', '')
-- in connector.yaml. The Confluence v2 /children endpoint reliably
-- returns pageId in our integration tests, but if the API ever omits
-- it, the silver staging model's `WHERE page_id IS NOT NULL AND
-- page_id != ''` filter would silently drop those rows from the
-- engagement aggregate.
--
-- This test makes the silent drop loud — it fires (returns rows) when
-- a reply has no resolvable page_id. If observed in production, debug
-- against the live API; if confirmed legitimate, switch the connector
-- to derive page_id from the parent's substream context instead of the
-- API response field.

SELECT
    'footer_reply' AS kind,
    unique_key,
    comment_id,
    parent_comment_id,
    tenant_id,
    source_id
FROM {{ source('bronze_confluence', 'wiki_footer_comment_replies') }}
WHERE page_id IS NULL OR page_id = ''

UNION ALL

SELECT
    'inline_reply' AS kind,
    unique_key,
    comment_id,
    parent_comment_id,
    tenant_id,
    source_id
FROM {{ source('bronze_confluence', 'wiki_inline_comment_replies') }}
WHERE page_id IS NULL OR page_id = ''

LIMIT 100
