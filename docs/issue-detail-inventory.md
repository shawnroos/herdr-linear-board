---
authors: Claude
type: inventory
project: herdr-board
topics: [linear, issue-detail, tui-design]
---

# Issue data inventory — what the board has about one Linear issue

First pass, read from `origin/main` @ 286ebb8 (2026-10-02). Purpose: input to the issue detail design.
Paths are relative to the repo root.

## At a glance

| Source | What exists | Shown today |
|---|---|---|
| Linear, board snapshot | identifier, title, url, state, priority, assignee, label names, stale flag | Card: ID, priority, title, assignee, `~stale` |
| Linear, single-issue read | + description, due, estimate, project, milestone, cycle, parent, sub-issues, relations, comments + replies, history | Issue page (main column + sidebar) |
| Linear, fetched then dropped | `branchName`, `team`, `updatedAt`, label parent groups, user ids, relation ids, history `from_*` | Nothing |
| Board local state | marks (4 kinds, owner, time, detail), notes, show requests (outcome, TTL), worktree binding (branch, bound_at, carried), agent activity log | Marks (card + page), newest note (card), pending show request (board line) |
| herdr runtime | bound worktree, tab, panes + live status (idle / working / blocked / done); agent name, title, cwd available but not projected | Card `▶` + `⧉N`; page Board block |
| Git / GitHub / CI | **Nothing.** No PR, diff, CI or preview source exists | — |

## 1. Linear — fetched today

Two reads (`crates/board-daemon/src/linear/queries.rs`):

- **Board snapshot** — `issues(...){ISSUE_FIELDS}` (:28-33), cached per (herdr session, space), reduced to `LinearIssue` (`board-core/src/protocol.rs:2023-2044`).
- **Issue page** — `issue(id){ISSUE_FIELDS ISSUE_DETAIL_FIELDS}` (:138-144), op `linear.issue`, returns `LinearIssueDocument` (`protocol.rs:1177-1188`) with `status` ok / partial / unavailable and `truncated` naming connections that hit the 50 cap.

### ISSUE_FIELDS (both reads)

| Field | Snapshot | Detail | Shown |
|---|---|---|---|
| `identifier` | yes | yes | Card line 1; page header |
| `title` | yes | yes | Card; page main column |
| `url` | yes | yes | Not rendered; `u` opens the snapshot url |
| `state {id name type}` | yes | yes | Column placement; sidebar Status |
| `priority` | yes | yes | Card `P{n}`; sidebar |
| `assignee {id name}` | yes | yes | Card `@name`; sidebar |
| `labels(first:10)` | names only | names only | Sidebar only; parent group dropped; cap 10, no truncation flag |
| `parent {identifier title}` | dropped | yes | Sidebar Parent |
| `project`, `projectMilestone`, `cycle` | dropped | yes | Sidebar |
| `team {key name}` | dropped | dropped | No |
| `branchName` | dropped | dropped | No |
| `updatedAt` | sort only | yes | No |

### ISSUE_DETAIL_FIELDS (issue page only)

| Field | Shown |
|---|---|
| `description` (Markdown) | Main column under title |
| `dueDate`, `estimate` | Sidebar |
| `children(first:50)` with state | Main "Sub-issues done/total", rows open with Enter |
| `relations` + `inverseRelations` (first:50) | Sidebar Relations; only `blocks` gets a direction-aware label |
| `comments(first:50)` with replies, author, time | Main Activity, after history |
| `history(first:50)` state / assignee / priority / label changes | Main Activity, phrased from the *to* side only |

## 2. Linear — available but not fetched

- Attachments (where Linear keeps GitHub PR links), reactions, subscribers, previews — explicitly out of scope in `docs/plans/2026-09-18-0903-feat-issue-detail-parity-plan.md`.
- Writes (status, assignee, comments) — out of scope; the daemon client is read-only.
- Anything beyond the caps (50 per connection, 10 labels).
- Already fetched but dropped in mapping: `team`, `branchName`, label groups, user ids, relation ids.

## 3. Board local state (SQLite v17, `schema.sql:186-330`, read via `linear.state.get`)

| Table | Fields | Shown |
|---|---|---|
| `linear_marks` | kind (`!` attention, `?` question, `✓` done, `◇` suggestion), text, detail JSON, created_by, created_at, owner (herdr socket + pane + Claude session) | Card gutter; page "Marks" as of open; opening clears all but `◇`. Owner, time, detail not shown |
| `linear_notes` | body, author, created_at, owner | Card shows newest note `› body`; **not on the page** |
| `linear_show_requests` | reason, requested_by, created_at, acknowledged_at, expires_at (30 min), outcome (accepted / rejected / withdrawn / expired) | Board bottom-bar alert; **not on the page**; outcomes never shown |
| `linear_worktree_bindings` | worktree_path, state (bound / misplaced / stale), **branch**, tab, display_name, carried JSON, bound_at | Page Board block shows path + state; branch, display_name, carried, bound_at not shown (status line shows elapsed) |
| `linear_activity` | agent tool calls with claims (socket, pane, workspace, card, run), last 500 per space | Not shown in the TUI |

Legacy `cards` / `comments` / `runs` are not linked to Linear issues.

## 4. herdr runtime

| Field | Shown |
|---|---|
| Bindings → worktree, tab, panes (`ops/linear/native.rs:348-419`) | Page Board block; `y` copy path, `o` focus pane, `b` bind |
| Pane status idle / working / blocked / done / unknown | Card `▶` / `⧉N`; page pane rows (cursor starts on the working pane) |
| Available in herdr's session snapshot, not projected: agent name, pane title, cwd, agent session, tab agent status | No |

## 5. Git / GitHub / CI

None. The only git call is `git rev-parse --show-toplevel` (`board-core/src/scope.rs:84`). Branch data exists in two places and neither is shown: the binding's `branch` column and Linear's `branchName` (fetched, discarded). **The toy's PR, preview, diff and CI rows have no real source today.**

## 5b. Decision: PR, diff and CI data come from Linear (Shawn, 2026-10-02)

Checked against the live Linear API (schema introspection plus real data on AI-575, 2026-10-02):

| Data | Where in Linear | Reachable from an issue today? |
|---|---|---|
| Linked PRs: number, title, url, repo, branch → target, status (open / merged / closed), draft, hasConflicts, reviews (state, reviewer, bot), reviewers, created / merged / closed times, linkKind (`contributes` …) | `Issue.attachments` → `Attachment.metadata` where `sourceType = "github"` | **Yes**, one extra field on the issue read |
| Issue `branchName`, `startedAt`, `completedAt`, SLA times | `Issue` | **Yes** |
| CI checks: name, workflowName, status, url, isRequired, started / completed | `PullRequest.checks` (`PullRequestCheck`) | **No public path found.** `PullRequest` is only reachable via `AgentSession.pullRequest(s)` or `Query.diff(id)`; `diff(id)` rejected the PR url, slug and id we have |
| Preview links: name, url, isBuilding | `PullRequest.previewLinks` | Same as checks |
| Diff stats, mergeStatus, isBehind, autoMerge, stacks | `PullRequest` / `Diff`; the Linear connector's `list_diffs` / `get_diff` return stats + mergeStatus + reviewers | Same as checks via the API key; the connector shows stats but not checks |

Same finding recorded independently in civitaspo/herdr-linear-agent PR #57.

### Final split (Shawn, 2026-10-02): Linear for the issue and its PR list, `gh` for everything git-related

Linear's attachments say **which** PRs belong to the issue (repo + number + url). `gh` fills in the git detail for each PR. Verified on slateteams/web-app#6352 (AI-575):

| Field | `gh` source | Example |
|---|---|---|
| State, draft | `gh pr view <url> --json state,isDraft` | OPEN, false |
| Mergeable / why blocked | `--json mergeable,mergeStateStatus` | MERGEABLE, BLOCKED |
| Review decision + latest reviews | `--json reviewDecision,latestReviews` | coderabbitai COMMENTED |
| Diff stats | `--json additions,deletions,changedFiles` | +352 −87, 14 files |
| Branches | `--json headRefName,baseRefName` | task/cue-jobs-motion → develop |
| CI checks | `--json statusCheckRollup` (CheckRun: name, conclusion, detailsUrl; StatusContext: context, state) | 40 entries; re-runs repeat names, so dedupe to the latest per name. The blocker here was the required status `E2E Verification Required: PENDING` |
| Preview URL | `gh api repos/<o>/<r>/deployments?ref=<branch>` → latest → `/statuses` → `environment_url`, `state` | https://pr-6352.preview.stage.slateteams.com, success |

Notes for the build: `gh` runs per PR, so cache per (repo, number, headSha) and refresh on a timer or when the board refreshes. `gh` auth comes from the user's machine, as it does for the board's other `gh` uses (none in the daemon yet).

## 6. Today's issue view (`crates/board-tui/src/view/linear_issue.rs`)

- Opens as a borderless window (≤ 128 wide) over the faded board at ≥ 136 cells, else over the whole board; `e` expands, `o` pins it beside the board, `p` focuses the pin.
- Header: powerline `ID │ status`, `project › milestone`, buttons; the bottom bar under it; body inset 2 cells.
- Main (100-cell cap): title → description → ATTACHMENTS (cards) → PULL REQUESTS (open cards with `linear.pr` data, merged lines) → SUB-ISSUES → ACTIVITY.
- Sidebar (40 cells, from 100 cells wide): properties → Parent → Relations → Marks → Board.
- Keys: j/k (rows, then scroll), PgUp/PgDn, Enter (linked issue with back stack, pane focus, link), e, o, u, y, b, r, Esc / Backspace / ←.

## Gaps worth deciding on

1. **Bug-like:** Status, Priority, Assignee, Labels and the `u` URL come from the board snapshot, not the detail read. A linked issue that isn't on the board shows `—` and `u` fails, even though the detail read has the data.
2. Notes, show requests (with outcomes) and agent activity exist but never reach the page.
3. Branch is known twice but never shown.
4. PR / diff / CI need a new source: Linear attachments (cheapest — same API), or `gh` against the worktree's branch.
5. herdr already reports which agent runs in each pane and its title; the page shows only pane id + status.
6. Binding state `worktree_missing` is checked by the TUI but never emitted by the native snapshot.
