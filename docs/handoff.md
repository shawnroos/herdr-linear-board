# Spinoff: herdr-linear-board as the interface for the work plugin

> This handoff is directional — author intent and a starting point, not a spec.
> The code and tests are the source of truth; validate against them and refine.

## Goal

Repurpose the `herdr-linear-board` fork into a visible interface for the `work`
plugin. The board takes its state from the herdr space it is open in, rather than
from its own database.

Shawn's words: "a kind of interface for this plugin. Getting its state from the
herdr space it is in."

## Why now / context

The `work` plugin just learned to place work by a premise: **herdr state follows
the Linear model**.

| Herdr | Linear | Recorded where |
|---|---|---|
| space | project | `~/.claude/work/workspaces/<space-id>.json` |
| tab | a piece of work, one ticket | the ticket's binding record |
| pane | a session on that work | herdr itself |

That model has no view. Everything the plugin knows lives in JSON records and a
shadow log. A board that opens inside a space and shows that space's tickets, their
worktrees, and their sessions is the obvious missing surface.

`herdr-board` is already most of the chassis. It is a herdr 0.9.0 plugin that opens
as an overlay pane, it reads which space and pane it was opened from, and it already
dispatches agents into visible panes.

## Facts established before this spinoff

- **The fork is untouched.** It is 0 commits ahead of and 0 behind its parent,
  `nelsonPires5/herdr-board`, at v0.17.0. Despite the repo description "Linear boards
  in herdr", nothing is repurposed yet.
- **Upstream is active.** v0.17.0 shipped on 2026-09-13 with herdr 0.9.0 support.
- **It is Rust**, five crates: `board-cli`, `board-core`, `board-daemon`,
  `board-herdr`, `board-tui`. One binary, `board`, is the TUI, the daemon and the CLI.
- **It has its own source of truth.** A `boardd` daemon owns an SQLite store at
  schema v15 (`schema.sql`): projects, boards, columns, cards, comments, runs.
- **Its runtime pins exactly herdr 0.9.0 and socket protocol 22.** The local machine
  runs herdr 0.9.0 on protocol 22, so it is compatible today. A herdr upgrade breaks it.
- **It already knows its context.** Scope selection reads
  `HERDR_PLUGIN_CONTEXT_JSON.focused_pane_cwd`, then `workspace_cwd`, then the process
  cwd. That is the natural hook for "state from the space it is in".
- **The work plugin's live store is sparse.** One bound space, two worktree bindings,
  zero repository records. PR #82, which adds the repository records and the
  space-and-tab placement, is open and unmerged. A board reading the store today
  shows almost nothing.

## Key decisions already made (in the work plugin — do not relitigate)

- **Bindings are authoritative; labels never are.** On Shawn's machine the only bound
  space is labelled `Editor` and carries the project `Example Launch`. A board
  that matches a space to a project by label shows the wrong project.
- **One resolution rule** for team, repository, space and tab. Exactly one known
  answer: resolve it and state the fact and its source. More than one: ask, naming
  every candidate. None: ask and record the answer.
- **Writes to Linear go through a consent gate.** A write with no recorded answer for
  that directory goes to a shadow log and records a notice instead of reaching
  Linear. A person can decline. Nothing records consent on the agent's own initiative.
- **A hook never binds or chooses.** It has nobody to ask.

## The collisions this work has to resolve

These are the real design questions. Each is a place where two systems each
believe they own the same thing.

1. **Source of truth.** The board's SQLite versus the work plugin's store plus Linear.
   "State from the space" points at the work plugin's records. Is the SQLite store
   kept as a cache, reduced to board-only concerns such as column layout, or removed?
2. **Tabs.** The board opens one `card-<id>` tab per card. The work plugin records one
   tab per ticket and never infers it from a label. Two tab conventions in one space
   produce duplicate tabs for the same work.
3. **Scope.** The board's project is a Git root or a cwd. The work plugin's scope is a
   Linear project bound to a space, and one project spans several repositories.
   These are different keys for different things.
4. **Placement.** The board dispatches an agent when a card moves into an `auto`
   column. The work plugin opens sessions in the ticket's tab. Two placers means two
   sets of rules for where a session lands. One has to call the other.
5. **Write authority.** If moving a card changes a Linear state, that write has to
   pass the work plugin's consent gate. A board that writes to Linear directly is a
   second, ungated write path — the exact thing the gate exists to prevent.
6. **Upstream.** Keep merging from `nelsonPires5/herdr-board`, or diverge into a hard
   fork? Upstream is active, and every divergence makes the next merge harder. A thin
   adapter layer keeps the option open; a rewrite of the data model closes it.

## Open questions / not yet decided

- Is the board a **view** of work-plugin state, a **controller** that calls work-plugin
  verbs, or both?
- How does a Rust binary read the work plugin's state: parse the JSON records
  directly, shell out to the `lib/` verbs, or does the work plugin need a stable
  read-only interface first?
- What is a "card" when the source of truth is Linear: an issue, a worktree binding,
  or a session?
- What does the board show in a space with no binding? Under the one resolution rule
  it proposes a binding and asks. It does not guess.

## Starting point

**This repo** (`~/projects/herdr-linear-board`, branch from `origin/main`):
- `docs/design.md` — §1 Concepts, §3 Data model, and the Scope selection section
  near line 416.
- `docs/herdr.md` — how the board talks to herdr, including placement and env.
- `crates/board-herdr` — the herdr client.
- `herdr-plugin.toml` — the overlay pane wiring.
- `schema.sql` — the store this work competes with.

**The work plugin** (`~/projects/shrimpshack`, branch `feature/work-plugin-initiative`,
PR #82):
- `CONCEPTS.md` — the vocabulary: Binding, Unbound, Misplaced, Stale.
- `plugins/work/lib/binding.sh` — `workspace_read`, `workspace_state`,
  `workspace_project`, and the record format.
- `plugins/work/lib/herdr-read.sh` — `workspace_id`, `tab_id`, `panes_in_tab`.
- `plugins/work/lib/context.sh` — the one resolution rule to mirror.
- `docs/plans/2026-09-11-0753-refactor-ticket-derived-worktree-location-plan.md` —
  KTD12 to KTD14 and U7, the placement design.

**A sibling spinoff overlaps.** `feature/work-plugin-config` in the shrimpshack repo is
designing a config file for the work plugin, including how spaces, tabs and splits
map and how they are named. A board that renders that mapping depends on it.
Coordinate rather than invent a second naming scheme.

## Recommended next step

`/ce-brainstorm`. The hard part is deciding which system owns what — source of
truth, tab convention, placement, and write authority — not writing Rust. Five of
the six collisions above are ownership questions, and the upstream question changes
how much of the board it is sane to touch. Scope that first, then `/ce-plan`.

## Source session

Transcript: `/Users/shawnroos/.claude/projects/-Users-shawnroos-projects-shrimpshack-worktrees-work-plugin-initiative/2b955433-71c5-4679-8ff9-e8d925d5a9f5.jsonl`
Resume:     `cd /Users/shawnroos/projects/shrimpshack/worktrees/work-plugin-initiative && claude -r 2b955433-71c5-4679-8ff9-e8d925d5a9f5`
