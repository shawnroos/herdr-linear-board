# Decision: the board owns the work store

**Status:** Proposed. Nothing described here is built.
**Date:** 2026-09-28
**Reopens:** [design.md](design.md) §9 item 7 ("No MCP — CLI only"), the "no MCP needed" line in §10, and the
"never writes … a plugin record or a SQLite row" contract in §13.

This record argues and decides. It does not plan the migration; a separate plan does that. The
open questions that plan must answer are at the end.

## Context

Two programs show the same Linear work, and they were never joined.

- **They group by different models.** The work plugin (`work@shrimpshack`) groups by `board.json`:
  4 levels (space, tab, column, row) by 8 fields (team, project, milestone, cycle, assignee, state,
  priority, parent), plus `label-group:<name>`, `ticket` and `sub-ticket`, with per-space
  overrides and a filter. The board groups on one flat axis (`LinearSnapshot.groups` in
  `crates/board-core/src/protocol.rs`), taken from a Linear custom view. A custom view cannot
  express the level-by-field model.
- **The wire between them ignores the plugin's own config.** boardd runs five plugin scripts
  (`crates/board-daemon/src/ops/linear.rs:29-33`, spawned at `:594`). The snapshot script hardcodes
  its mapping at shrimpshack `plugins/work/bin/work-snapshot.sh:419`
  (`"source": "default", "space": "project", "tab": "work", "pane": "session"`) and never reads
  `board.json`. So `board.json` drives the plugin's herdr sync, the script feeds the TUI, and
  nothing connects the two.
- **The store has four writers inside one plugin.** `~/.claude/work` holds `bindings`, `board`,
  `board.json`, `descriptions`, `layouts`, `scopes`, `shadow.log`, `workspaces` and
  `write-enabled`. Four separate record engines write it (shrimpshack `lib/record.sh`,
  `lib/repos.sh`, `lib/board-store.sh`, `lib/board-config.sh`), each with its own checks and a
  write-then-rename save, sharing one `mkdir` lock. `lib/board-sync.sh:82-115` adds a second lock.

Fixing the snapshot script to read `board.json` would close the display gap. It would also leave
two programs owning one set of data, which is the actual problem.

The original research planned for this step. [research.md](research.md) line 161 chose a tiny CLI
over MCP for v1 and said "MCP wrapper later"; line 160 said "JSON/md files race with concurrent
writers" and put SQLite behind a single daemon writer. This record is that "later" for MCP, and it
applies line 160's rule to the plugin's store.

## Decision

### Two MCP servers, two jobs

Agents already have Linear's official MCP server. The board does not compete with it.

| | Linear MCP (official) | `board mcp` |
|---|---|---|
| Owns | Linear objects: issues, comments, states, projects | The links between Linear objects and local work: which session, pane, tab and worktree belong to which issue |
| Agents use it to | create and update tickets | ask what they are bound to, bind or unbind, mark a card, notify the person |
| Writes approved by | Claude Code's tool permissions for Linear | Claude Code's tool permissions for `board` |

The board does not write Linear. An agent that creates or updates a ticket does it through Linear
MCP, and the board learns about it (see "Watching Linear MCP").

### One owner of local state

boardd becomes the only owner and the only writer of the work store. The store holds local state
only: bindings, scopes, layouts, and the grouping config. It is not a copy of Linear. It becomes
rows in boardd's SQLite database (today schema v15, `schema.sql`). Nothing else writes those rows.

The board has three clients. None of them owns data:

| Client | Used by | Door |
|---|---|---|
| TUI | Shawn | the existing board socket |
| `board mcp` | Claude | a stdio MCP server that forwards to the board socket |
| `board` CLI | scripts and hooks | the existing board socket |

The herdr panes are where boardd dispatches work. They are not clients.

The CLI and its skill ([design.md](design.md) §10) stay. Scripts and hooks need a door that is not
MCP.

### The board reads Linear itself, read-only

To draw the grouped board, boardd reads Linear through its GraphQL API with the API key from the
macOS Keychain, as the plugin does today (shrimpshack `lib/linear.sh`, `lib/secrets.sh`). The read
is read-only. It cannot go through Linear MCP: boardd is a background process, not a Claude
session, and Linear MCP does not guarantee the fields the grouping model needs.

So a person's machine has two Linear connections: the agent's, through Linear MCP, for writes; and
the board's, through GraphQL, for display.

### The grouping model moves into the board

The board adopts the plugin's grouping model: 4 levels by 8 fields, per-space overrides, and a
filter. A Linear custom view becomes the fallback for a space with no grouping configured. It
stops being the source.

### Watching Linear MCP

An MCP server cannot see another server's calls; only Claude Code sees every tool call. So the
board watches Linear MCP through a Claude Code PostToolUse hook, not through an MCP tool.

The plugin already has this hook. `plugins/work/hooks/board-behind.sh` (shrimpshack) fires after any
Linear MCP tool, treats every tool not named `get_`, `list_`, `search_` or `extract_` as a write,
and marks the board behind. Under this decision the hook stays thin: it passes the tool name, its
input, its result and the session's working directory to boardd with one `board` CLI call, and
exits 0 on every failure.

With that report, boardd can:

- **Refresh** the board after every Linear write, instead of waiting for a manual refresh.
- **Link by observation.** When an agent in a pane the board knows creates ENG-123, boardd can link
  that session to the issue without the agent being told to.
- **Keep history.** boardd can list which sessions touched an issue, when, and with which tool.

Limits: hooks are Claude Code only, so agents in other harnesses (Pi, Codex) link only through
the explicit `bind` tool. Linear MCP tool names differ by install (`mcp__linear__save_issue` in one
setup, a `claude_ai` prefix through the claude.ai connector), so the hook keeps the plugin's broad
matcher.

### What `board mcp` offers

`board mcp` is a subcommand of the one `board` binary. It holds no state. It translates MCP tool
calls into board socket requests.

| Tool kind | Examples | Marked as |
|---|---|---|
| Read | what am I bound to; show the panes, tab and worktree for an issue; show the board for a space | read-only |
| Link | bind this session to an issue; unbind | write |
| Mark | flag a card "needs you", "done" or "has a question"; attach a short note to a card | write |
| Notify | send a herdr notification ("ENG-123 is ready for review") | write |
| Ask to show | ask the person to look at an issue: the board shows "Claude wants to show you ENG-123 — press enter" | write |

Agents point; they do not grab the person's view. No tool opens the board, focuses a pane, moves
the cursor or jumps the TUI to a screen. [skill/SKILL.md](../skill/SKILL.md) already refuses a CLI
verb for focusing a pane because "it moves the person's view in herdr, which an agent has no
reason to do", and this keeps that rule. An agent in a background pane would take focus while the
person types, and several agents would fight over one view. "Ask to show" moves the view only when
the person presses the key.

Marks, notes and show-requests reach the TUI the way board changes already do: the tool call goes
to boardd, boardd records it, and the TUI draws it from an event. Agents never drive the TUI
through herdr keystrokes.

- **Finding it.** The user installs the server once, at user scope, for example
  `claude mcp add --scope user board -- board mcp`. Only the user can install it. The board writes
  no `.mcp.json` into the worktrees it creates: user scope covers every Claude session, and a
  project file would need an approval per worktree and dirty every worktree.
- **Knowing who is calling.** An agent the board spawned already carries `HERDR_PANE_ID`,
  `BOARD_CARD_ID` and `BOARD_RUN_ID` in its environment, and `board mcp` inherits them, so a tool
  call arrives attributable to a card, a tab and a space. Paper and Open Design are GUI apps that
  are also MCP servers and default their tool calls to the user's current context; the board gets
  a stronger version because it started the agent. These variables are claims the caller sets, not
  proof. A rescued pane has an empty `BOARD_RUN_ID` on purpose, so its calls attribute to the card
  only.

### Approval lives in the tool permissions

Claude Code's permission prompt is the approval for every write. Each `board mcp` write tool is
its own permission entry, so the person decides per tool whether to allow it always or be asked
each time. Every write tool returns exactly what it changed, so the agent's transcript shows it,
and every link can be undone (`unbind`). The TUI shows current state; it has no approve step.

The trade-off: an agent running with auto-approved permissions links, marks and notifies without
asking anyone. That is acceptable because these writes are local and undoable. Linear writes go
through Linear MCP, under the permissions the person set for it.

This replaces the plugin's propose/confirm nonce for bindings (shrimpshack `lib/record.sh`,
`lib/board-store.sh`, `lib/binding.sh`). The plugin's own notes say that nonce orders a write and
does not prove a person saw it (`lib/binding.sh:12-27`), so dropping it loses no guarantee the
plugin actually had.

### Failure model

A stopped daemon is not an outage. `connect_or_start` (`crates/board-cli/src/daemon.rs:16-35`)
starts boardd when it is absent, by re-launching its own binary through `current_exe()`, not
through `PATH`. `board mcp`, the hook and any user-installed command that calls `board` get the
same behaviour. `board mcp` keeps stdout for the MCP stream and never prints through
`crates/board-cli/src/render.rs`.

The real failure is a missing or unfindable `board` binary. Then the MCP server does not start,
the hook's report is lost (the hook still exits 0, so the agent is not blocked), and every
command that calls `board` fails. Linear MCP keeps working, because it does not depend on the
board.

### Protocol changes stay additive

New fields are added, existing fields are never re-typed, and the version is bumped, so an older
client against a newer daemon sees a narrower result instead of failing to parse. This applies to
boardd's own protocol ([protocol.md](protocol.md), v1).

The Linear document `schema` field stops being a cross-process contract once boardd reads Linear
itself; it matters only while the old plugin and the new board overlap. If that window needs a
schema bump, the two hard `schema != 1` rejects (`crates/board-daemon/src/ops/linear.rs:213` and
`:243`) must change together
([solution note](solutions/integration-issues/a-pinned-version-check-has-a-twin.md)). Any new
field that crosses a process boundary needs `Option<T>` or `null_as_empty`
([solution note](solutions/integration-issues/serde-default-rejects-explicit-null.md)). Today
`LinearGroup` and the snapshot types have neither.

## What this retires

- `plugins/work/bin/work-snapshot.sh` and its hardcoded mapping, and the four other bin scripts
  boardd runs (`work-spaces.sh`, `work-projects.sh`, `work-views.sh`, `work-issue.sh`).
- The plugin's own Linear write path: its create, update, description and document commands, and
  the guards around them (shadow mode, `write_allowed`, per-directory write consent). Agents write
  through Linear MCP instead.
- The propose/confirm nonce for bindings.
- The cross-repo fixture contract: the vendored documents under
  `crates/board-core/tests/fixtures/linear-snapshot/` and `linear-issue/`, and the sha256 `VERSION`
  pin that both repos assert ([design.md](design.md) §13, "Fixture contract").
- Display parity as a wire problem. The TUI and Claude read the same rows at the same moment.
- The four record engines and both locks in `~/.claude/work`.
- The plugin's bash-only test gates, including the library-sourcing gate (shrimpshack PR #94).
- In [design.md](design.md) §13: "The board never moves a card, edits an issue, or writes a Linear
  object, a plugin record or a SQLite row", "the daemon has no row for it", and "`board_changed`
  events are ignored, since no board row can change". The board still never writes a Linear
  object.

## Consequences

- **Test cost.** The plugin's suite is 38 bats files, 18,582 lines and 1,303 tests. Because the
  Linear write path retires, the largest test class mostly becomes obsolete instead of being
  ported. The per-class inventory is in
  [board-owns-the-store-tests.md](board-owns-the-store-tests.md).
- **Safety checks become guidance.** Shadow mode, the write-consent guard and the description
  template checks stop being enforced, because the board no longer sits between agents and Linear.
  Any worth keeping become instructions in the skill.
- **One binary on the path.** The board's doors depend on the `board` binary being installed and
  findable. Linear MCP does not.
- **The plugin shrinks** to a thin hook and whatever skills still add value.

## Rejected options

- **Make `work-snapshot.sh` read `board.json`.** Fixes the display gap and keeps two owners of the
  same data.
- **The board as the agents' Linear client.** Competes with the official Linear MCP server that
  users already have, and forces the board to own consent for Linear writes it cannot actually
  enforce against a same-user agent.
- **Keep the store as JSON with boardd as the only writer.** Keeps four file formats and the lock
  logic for no benefit over the database boardd already owns.
- **The plugin and the board both writing the store.** Two writers on one store is the failure mode
  this record exists to design out.
- **TUI confirmation for bindings.** Costs a round-trip to the TUI to guard local, undoable links;
  the tool permission prompt already asks the person when they want to be asked.
- **Agent tools that move the person's view.** Background agents would take focus while the person
  types.
- **An MCP tool that watches Linear MCP.** An MCP server cannot see another server's calls.
- **A `.mcp.json` written into each worktree.** Needs an approval per worktree and dirties every
  worktree; user scope covers every session.

## Related

A small board status pane built on Claude Code function hooks (current card, run elapsed, agents
running, show-requests and "needs you" marks) is a separate proposal. It overlaps this record in
one place: the marks and show-requests agents send are what such a pane would show.

## Open questions for the migration plan

- Where user-authored grouping config lives: rows in SQLite, or a section of the board's existing
  TOML config (`RootConfig` in `crates/board-core/src/config.rs`).
- Which package ships the Linear MCP hook once the plugin shrinks: the plugin, the board's optional
  skill, or a small Claude Code plugin of its own.
- Whether linking by observation happens automatically, or is offered as a suggestion the agent or
  person accepts.
- Whether a hook that runs before a Linear write can fill in the bound project or team on a new
  ticket. That depends on whether Claude Code lets a PreToolUse hook change a tool's input, which is
  not yet verified.
- Cut-over: whether the plugin reads through the board during a transition, or the move ships in one
  release.
- The plugin's herdr fakes disagree: `tests/fixtures/fake-herdr.sh` reports herdr 0.8.2 / protocol
  20 in its status output but 0.9.0 / protocol 22 in its canned snapshot, while
  `tests/fixtures/fake-herdr-socket.py` pins 0.9.0 / protocol 22. Which ported tests inherit which,
  given boardd accepts only 0.9.0 / protocol 22.
