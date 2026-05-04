"""HubSpot stream registry, property-type mapping, and API limits."""

from typing import FrozenSet, Mapping

# ------- API -----------------------------------------------------------------

BASE_URL = "https://api.hubapi.com"

# ------- Search endpoint caps ------------------------------------------------

# HubSpot Search returns HTTP 400 once `after >= SEARCH_AFTER_HARD_CAP`.
# Hit this, switch to keyset pagination within the same time slice.
SEARCH_AFTER_HARD_CAP = 10_000

# Search endpoint: 100 records per page is the maximum the API accepts.
SEARCH_PAGE_LIMIT = 100

# v3 list endpoints: 100 records per page (same cap as search).
LIST_PAGE_LIMIT = 100

# v4 associations batch_read accepts up to 1000 ids per call, but 100 keeps
# request bodies small enough that a 429 retry doesn't replay a big payload.
ASSOCIATIONS_BATCH_SIZE = 100

# ------- Property-type mapping (describe -> JSON schema) ---------------------

# HubSpot property type -> (json-schema type, optional format).
# Any type not listed falls back to string with a one-time warning.
HUBSPOT_TYPE_TO_JSON_SCHEMA: Mapping[str, tuple] = {
    "string": ("string", None),
    "bool": ("boolean", None),
    "boolean": ("boolean", None),
    "enumeration": ("string", None),
    "date": ("string", "date"),
    "datetime": ("string", "date-time"),
    "date-time": ("string", "date-time"),
    "number": ("number", None),
    "json": ("string", None),
    "object_coordinates": ("string", None),
    "phone_number": ("string", None),
}

# ------- Cloudflare oddity ---------------------------------------------------

# HubSpot fronts the API via Cloudflare; an invalid token format (e.g. wrong
# prefix) bubbles up as a 530, not a proper 401. Map it to a config error with
# a token-format hint.
CLOUDFLARE_ORIGIN_DNS_ERROR = 530

# ------- Curated stream registry ---------------------------------------------

# ``object_type`` is the path segment used with /crm/v3/objects/{object_type}
# and /crm/v3/objects/{object_type}/search. ``primary_key`` is always "id".
# ``associations`` lists object types to co-fetch via v4 batch_read.
#
# Operator can override the active set via config.hubspot_streams.
STREAM_REGISTRY: Mapping[str, Mapping] = {
    # Contacts: full_refresh because hs_lastmodifieddate doesn't bump on
    # engagement activity (calls/emails logged against contacts don't touch
    # the contact's properties), so incremental returns 0 records on portals
    # where reps log activities but rarely edit contact records directly.
    # Full refresh re-pulls all contacts each sync — keyset pagination
    # handles the 10k search-result cap within the single big window.
    "contacts": {
        "object_type": "contacts",
        "cursor_field": None,
        "search_cursor_property": "hs_lastmodifieddate",
        "associations": ["companies", "deals"],
        "silver_tag": "silver:class_crm_contacts",
    },
    "companies": {
        "object_type": "companies",
        "cursor_field": "updatedAt",
        "search_cursor_property": "hs_lastmodifieddate",
        "associations": [],
        "silver_tag": "silver:class_crm_accounts",
    },
    "deals": {
        "object_type": "deals",
        "cursor_field": "updatedAt",
        "search_cursor_property": "hs_lastmodifieddate",
        "associations": ["companies", "contacts"],
        "silver_tag": "silver:class_crm_deals",
    },
    "engagements_calls": {
        "object_type": "calls",
        "cursor_field": "updatedAt",
        "search_cursor_property": "hs_lastmodifieddate",
        "associations": ["contacts", "companies", "deals"],
        "silver_tag": "silver:class_crm_activities",
    },
    "engagements_emails": {
        "object_type": "emails",
        "cursor_field": "updatedAt",
        "search_cursor_property": "hs_lastmodifieddate",
        "associations": ["contacts", "companies", "deals"],
        "silver_tag": "silver:class_crm_activities",
    },
    "engagements_meetings": {
        "object_type": "meetings",
        "cursor_field": "updatedAt",
        "search_cursor_property": "hs_lastmodifieddate",
        "associations": ["contacts", "companies", "deals"],
        "silver_tag": "silver:class_crm_activities",
    },
    "engagements_tasks": {
        "object_type": "tasks",
        "cursor_field": "updatedAt",
        "search_cursor_property": "hs_lastmodifieddate",
        "associations": ["contacts", "companies", "deals"],
        "silver_tag": "silver:class_crm_activities",
    },
    "leads": {
        "object_type": "leads",
        "cursor_field": "updatedAt",
        "search_cursor_property": "hs_lastmodifieddate",
        "associations": ["contacts", "companies"],
        "silver_tag": None,  # bronze-only in v1
    },
    "tickets": {
        "object_type": "tickets",
        "cursor_field": "updatedAt",
        "search_cursor_property": "hs_lastmodifieddate",
        "associations": ["contacts", "companies", "deals"],
        "silver_tag": None,  # bronze-only in v1
    },
    # owners is NOT a CRM object — different endpoint shape (/crm/v3/owners).
    # Handled by a dedicated stream class; no search endpoint, no properties.
    # cursor_field=None forces FinalStateCursor (full-refresh). The owners
    # list endpoint doesn't accept an updatedAt filter, so time-slicing via
    # ConcurrentCursor would just re-read the full owner set per slice.
    "owners": {
        "object_type": "owners",
        "cursor_field": None,
        "search_cursor_property": None,
        "associations": [],
        "silver_tag": "silver:class_crm_users",
    },
}

# Curated default list — matches the streams listed in the plan.
CURATED_STREAMS = list(STREAM_REGISTRY.keys())

# ------- Standard property allowlist per object_type ------------------------
#
# HubSpot Properties API returns every property the portal has defined for
# an object — typically 200+. Listing them all in /search request bodies
# blows up Bronze schema width and the destination's binary-insert buffer
# (CH OOMs on the dedup/replace pass). Bronze stays narrow when we ask
# HubSpot only for a curated set of standard properties: those Silver dbt
# models reference today, plus a small forward-looking set picked for
# likely near-future analytics (segmentation, geo, win-loss, compliance).
#
# Custom (tenant-defined) properties — those with ``hubspotDefined=False``
# in HubSpot's properties API — are NOT listed here; they pass through
# api.property_names() and are bundled into the ``custom_fields`` JSON
# column by the envelope. Each tenant's customizations land there without
# code changes and without polluting the standard schema.
#
# When a Silver dbt model starts referencing a new ``properties_<name>``
# column, add the property name here for the corresponding object_type
# and re-sync. Otherwise the column will be missing in Bronze.
#
# Object types match HubSpot's URL segment, NOT our stream names — e.g.
# stream ``engagements_calls`` syncs object_type ``calls``.
ALLOWED_PROPERTIES_BY_OBJECT: Mapping[str, FrozenSet[str]] = {
    # Used by hubspot__crm_contacts.sql (8) + geo/source/status (5).
    "contacts": frozenset({
        # dbt-referenced
        "email", "firstname", "lastname", "hubspot_owner_id", "lifecyclestage",
        # metadata: geo, lead status, source
        "city", "state", "country", "jobtitle", "phone",
        "hs_lead_status", "hs_analytics_source",
    }),

    # Used by hubspot__crm_accounts.sql (9) + lifecycle/contact/type (3).
    "companies": frozenset({
        # dbt-referenced
        "annualrevenue", "city", "country", "domain",
        "hubspot_owner_id", "industry", "name",
        "numberofemployees", "state",
        # forward-looking: account-level funnel, contact channel, segmentation
        "lifecyclestage", "phone", "type",
    }),

    # Used by hubspot__crm_deals.sql (12) + win-loss / triage / context (3).
    "deals": frozenset({
        # dbt-referenced
        "amount", "closedate", "dealname", "dealstage", "dealtype",
        "hs_analytics_source", "hs_deal_stage_probability",
        "hs_forecast_category", "hs_is_closed", "hs_is_closed_won",
        "hubspot_owner_id", "pipeline",
        # forward-looking: win-loss reasons, priority, qualifying notes
        "closed_lost_reason", "hs_priority", "description",
    }),

    # Used by hubspot__crm_activities.sql for call objects (6) + status (1).
    "calls": frozenset({
        "hs_call_direction", "hs_call_disposition", "hs_call_duration",
        "hs_call_title", "hs_timestamp", "hubspot_owner_id",
        "hs_call_status",
    }),

    # Used by hubspot__crm_activities.sql for email objects (5).
    # Bodies/HTML/headers intentionally NOT included — large per-row, no
    # current consumer; revisit when an AI/NLP use case lands.
    "emails": frozenset({
        "hs_email_direction", "hs_email_status", "hs_email_subject",
        "hs_timestamp", "hubspot_owner_id",
    }),

    # Used by hubspot__crm_activities.sql for meeting objects (7) +
    # external link / short notes (2).
    "meetings": frozenset({
        "hs_meeting_end_time", "hs_meeting_location", "hs_meeting_outcome",
        "hs_meeting_start_time", "hs_meeting_title",
        "hs_timestamp", "hubspot_owner_id",
        "hs_meeting_external_url", "hs_internal_meeting_notes",
    }),

    # Used by hubspot__crm_activities.sql for task objects (6) +
    # completion date (1).
    "tasks": frozenset({
        "hs_task_priority", "hs_task_status", "hs_task_subject",
        "hs_task_type", "hs_timestamp", "hubspot_owner_id",
        "hs_task_completion_date",
    }),

    # Owners API exposes its own native fields (email, firstName, lastName,
    # userId, archived) — no ``properties.*`` envelope. Empty allowlist; the
    # filter is a no-op for this object type.
    "owners": frozenset(),

    # Bronze-only in v1 — no Silver model exists yet. Bronze captures the
    # envelope only plus any tenant-custom properties via ``custom_fields``.
    # Populate when class_crm_leads / class_crm_tickets are designed.
    "leads": frozenset(),
    "tickets": frozenset(),
}
