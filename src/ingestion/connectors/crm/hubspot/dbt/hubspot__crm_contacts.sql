{{ config(
    materialized='incremental',
    incremental_strategy='append',
    schema='staging',
    engine='ReplacingMergeTree(_version)',
    order_by='(unique_key)',
    settings={'allow_nullable_key': 1},
    tags=['hubspot', 'silver:class_crm_contacts']
) }}

SELECT * FROM (
    SELECT
        tenant_id,
        source_id,
        unique_key,
        id                                              AS contact_id,
        properties_email                                AS email,
        properties_firstname                            AS first_name,
        properties_lastname                             AS last_name,
        properties_hubspot_owner_id                     AS owner_id,
        nullIf(arrayElement(
            JSONExtract(coalesce(associations_companies, '[]'), 'Array(String)'), 1
        ), '')                                          AS account_id,
        properties_lifecyclestage                       AS lifecycle_stage,
        toJSONString(map(
            'phone',            coalesce(toString(properties_phone), ''),
            'city',             coalesce(toString(properties_city), ''),
            'state',            coalesce(toString(properties_state), ''),
            'country',          coalesce(toString(properties_country), ''),
            'jobtitle',         coalesce(toString(properties_jobtitle), ''),
            'hs_lead_status',   coalesce(toString(properties_hs_lead_status), ''),
            'hs_analytics_source', coalesce(toString(properties_hs_analytics_source), ''),
            'archived',         toString(coalesce(archived, false))
        ))                                              AS metadata,
        CAST(map() AS Map(String, String))              AS custom_str_attrs,
        CAST(map() AS Map(String, Float64))             AS custom_num_attrs,
        createdAt                                       AS created_at,
        updatedAt                                       AS updated_at,
        data_source,
        coalesce(
            toUnixTimestamp64Milli(updatedAt),
            0
        )                                               AS _version
    FROM {{ source('bronze_hubspot', 'contacts') }}
)
{% if is_incremental() %}
WHERE _version > coalesce((SELECT max(_version) FROM {{ this }}), 0)
{% endif %}
