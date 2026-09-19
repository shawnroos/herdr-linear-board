---
title: serde default does not accept an explicit null from another process
date: 2026-09-17
category: integration-issues
module: board-core::protocol
problem_type: integration_issue
component: data_model
symptoms:
  - "The Linear project picker shows plugin unavailable and lists no projects"
  - "LinearListResult::from_value fails with invalid type: null, expected a string"
  - "Both repos' test suites pass, because each tests only its own fixtures"
root_cause: missing_validation
resolution_type: code_fix
severity: high
tags: [serde, json-null, cross-repo-contract, linear-list, work-plugin]
---

# serde default does not accept an explicit null from another process

## Problem

The board parses `linear.list` output that the work plugin's bash and Python scripts print. The plugin's projects script prints `"team_key": null` for a project with no team. The board's row struct had `team_key: String` with `#[serde(default)]`. One project with no team broke the whole projects list, so the picker lost every row.

## Symptoms

- The project picker shows "plugin unavailable" and no rows.
- Parsing fails with `invalid type: null, expected a string`.
- Nothing fails in either repo's own tests. The plugin's bats fixture expects the null. The board's tests built rows without nulls.

## What Didn't Work

- `#[serde(default)]` reads as "absent or null". It only covers a key that is absent. A key that is present with `null` still goes to the `String` deserializer and fails.
- `board_core::text::sanitise_json` walks the JSON before typing, but it cleans string contents and passes `null` through unchanged.
- The snapshot struct already typed its team key as optional; the list row structs, added later, did not follow it. A code review that read the plugin script next to the Rust struct found the mismatch; no test did.

## Solution

On branch `feature/linear-mode-pickers` (PR not open at the time of writing), in `crates/board-core/src/protocol.rs`:

- A field where "no value" means something to the caller becomes `Option<String>`:

```rust
// before
#[serde(default)]
pub team_key: String,
// after
#[serde(default)]
pub team_key: Option<String>,
```

- Every other `String` field on `LinearSpaceRow`, `LinearProjectRow` and `LinearViewRow` gets a null-tolerant deserializer, so callers keep a plain `String`:

```rust
fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[serde(default, deserialize_with = "null_as_empty")]
pub name: String,
```

The CLI table and the TUI picker render a `None` team key as empty.

## Why This Works

`Option<String>` accepts JSON `null` as `None`. `#[serde(default)]` still covers the absent key, because `deserialize_with` runs only when the key is present. `null_as_empty` reads the value as `Option<String>` and turns `None` into `""`, so absent, `null` and `""` all end up as the same empty string.

## Prevention

- Rule: for a struct parsed from JSON that another process or repo writes, give every field that producer could null either `Option<T>` or a null-tolerant deserializer. Do not rely on `#[serde(default)]` for null.
- Add one test per row shape that sets every non-`Option` field to `null` and asserts the parse succeeds. See `list_rows_read_a_null_string_field_as_empty` and `a_project_row_with_a_null_team_key_parses_as_no_team` in `crates/board-core/tests/protocol.rs`. Removing `deserialize_with` makes the first test fail with the original error.
- When a plugin's fixture contains `null`, grep the consuming struct for that field's type.

## Related Issues

- `docs/protocol.md` documents `team_key` as `null` for a project with no team.
- `docs/design.md` covers `Patch<T>`, which models `null` as "clear" for updates. That is a different meaning of null from this one: missing source data.
