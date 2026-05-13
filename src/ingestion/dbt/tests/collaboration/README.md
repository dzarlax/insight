# collaboration dbt tests

Singular SQL tests on `silver.class_collab_meeting_activity` and
`silver.class_collab_chat_activity`. Each file returns rows **that
represent a violation** — a test passes when zero rows are returned.

Run:
```bash
dbt test --select test_name:assert_meeting_duration_caps --profiles-dir .
```

## What's covered

| Test | What it catches |
|------|-----------------|
| `assert_meeting_duration_caps` | A `class_collab_meeting_activity` row where `video_duration_seconds` or `screen_share_duration_seconds` exceeds `audio_duration_seconds`. By construction this should never happen — audio = session length, video/screen-share are sessions gated by per-user flags. Issue #263 reference. |
