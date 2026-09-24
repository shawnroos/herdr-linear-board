# Spinoff: Issue detail parity with Linear

> This handoff is directional — author intent and a starting point, not a spec.
> The code and tests are the source of truth; validate against them and refine.

## Goal

Make the board's issue detail page (the card detail opened with Enter or a click in Linear mode) structurally identical to Linear's own issue page. Same sections, same order, same properties, so a person moving between the two finds things in the same place.

## Why now / context

The Linear mode pickers PR (shawnroos/herdr-linear-board#2) shipped a card detail view, but it is a thin overlay. It shows one meta line (state · priority · assignee), labels, URL, binding path/tab and the bound panes. Shawn wants the detail page to mirror Linear's issue page structure. The board is now running live in herdr and bind from the board works (after the herdr preview-version gate fix, commit 6acb8b1).

## Key decisions already made

- **Base is PR #2's branch (`feature/linear-mode-pickers`), not `main`.** Linear mode does not exist on `main` yet: PR #1 (read-only Linear mode) and PR #2 are stacked and unmerged. This spinoff stacks on PR #2.
- **The board never reads Linear directly.** All Linear data comes from the work plugin (shrimpshack, `plugins/work`) through the daemon: `linear.snapshot` runs `bin/work-snapshot.sh`, `linear.list` runs the list scripts. Any new field the detail page needs must come the same way (plugin script → daemon op → typed protocol struct → TUI).
- **Cross-process JSON rows must tolerate null.** See `docs/solutions/integration-issues/serde-default-rejects-explicit-null.md`: every field the plugin can print as `null` needs `Option<T>` or the `null_as_empty` deserializer.
- **Read-only.** The detail page shows Linear data and board bindings; it does not edit issues. Writes to Linear go through the plugin's consent gate, which is out of scope here.

## Open questions / not yet decided

- **What "structurally identical" covers.** Linear's issue page has a main column (title, description, sub-issues, activity/comments) and a properties sidebar (status, priority, assignee, labels, project, milestone, cycle, estimate, due date, relations). Which sections are in scope for a terminal UI, and how the two-column layout collapses at narrow widths (the board stacks below 72 cells).
- **Where the extra data comes from.** The snapshot's `LinearIssue` (`crates/board-core/src/protocol.rs`, `pub struct LinearIssue`) carries only id, identifier, title, url, state, assignee, priority, labels, stale and bindings. There is no description, comments, sub-issues, project, cycle, estimate or due date. Options: widen the snapshot (heavier: 250+ issues per space), or add a per-issue read (a new plugin script like `work-issue.sh <id>` plus a `linear.issue` daemon op) fetched when the detail opens. The per-issue read looks more likely; it needs a plugin version bump (the board's floor is `PLUGIN_VERSION_FLOOR = "0.4.0"`).
- **Markdown rendering** of the description and comments in ratatui, and how much of it is worth doing.
- **Where the board-only sections go** (bound worktree, tab, panes, `b` to bind): as a sidebar block, or below the Linear sections.

## Starting point

- Detail rendering: `crates/board-tui/src/view/linear.rs`, `fn draw_detail` and `fn detail_lines`.
- Detail keys and state: `crates/board-tui/src/app/linear.rs` (`detail_key`, `LinearState::detail_issue`).
- Snapshot types: `crates/board-core/src/protocol.rs` (`LinearSnapshot`, `LinearIssue`).
- Daemon script runner: `crates/board-daemon/src/ops/linear.rs` (shared `ScriptRunner`, envelope, version floor).
- Plugin side: `~/projects/shrimpshack/worktrees/work-snapshot-board/plugins/work` — `bin/work-snapshot.sh`, `lib/linear.sh` (GraphQL queries), `docs/snapshot.md` (the contract), `tests/fixtures/fake-linear.sh`. Plugin PR shawnroos/shrimpshack#88 is stacked on #86.
- Snapshot tests: `crates/board-tui/tests/linear/` (insta). Sandbox gates: `scripts/sandbox.sh prepare` once, then `scripts/sandbox.sh gates` (Docker via `colima start`).
- Plan for the previous round: `docs/plans/2026-09-16-0929-feat-linear-mode-pickers-and-layout-plan.md`.

## Recommended next step

`/ce-brainstorm`. The scope of "structurally identical" and the data source (a wider snapshot or a per-issue read) are real product and contract choices that span two repos, so settle them before planning.

## Source session

Transcript: `/Users/shawnroos/.claude/projects/-Users-shawnroos-projects-herdr-linear-board-worktrees-herdr-board-work-interface/550348e7-b627-43fb-97e8-aa9fbe3ef6ef.jsonl`
Resume:     `cd /Users/shawnroos/projects/herdr-linear-board/worktrees/herdr-board-work-interface && claude -r 550348e7-b627-43fb-97e8-aa9fbe3ef6ef`
