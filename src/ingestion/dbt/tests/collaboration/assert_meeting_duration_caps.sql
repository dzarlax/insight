-- #263 sanity guard: per-user-day, neither video nor screen-share duration
-- should ever exceed audio duration (which for both feeders represents the
-- maximum time the participant was in the meeting).
--
--   • insight_m365 — both video and screen-share are exact minute-of-X and
--     are by construction ≤ audio_duration (Microsoft Graph guarantees this).
--   • insight_zoom — video / screen-share = session length × per-participant
--     flag. Audio = session length unconditionally. So Zoom rows should
--     have video / screen-share ≤ audio for any participant who joined a
--     single session that day. A row where video > audio implies either a
--     stitching bug or a regression of the per-participant flag wiring.
--
-- The test fires (returns rows) when the invariant is violated. Mostly a
-- defensive guard so future refactors of the Zoom feeder don't silently
-- start emitting video_duration > audio_duration without anyone noticing.

SELECT
    data_source,
    tenant_id,
    person_key,
    date,
    audio_duration_seconds,
    video_duration_seconds,
    screen_share_duration_seconds
FROM silver.class_collab_meeting_activity FINAL
WHERE audio_duration_seconds IS NOT NULL
  AND audio_duration_seconds > 0
  AND (
       coalesce(video_duration_seconds, 0)        > audio_duration_seconds
    OR coalesce(screen_share_duration_seconds, 0) > audio_duration_seconds
  )
LIMIT 100
