# Install and optional setup

The install steps the [root README](../README.md) summarizes, plus everything optional around
them: a custom CLI directory, a Herdr keybinding, the harness integration, the agent skill, Linear
mode's agent tools and hook, and named Herdr sessions.

Requires **Herdr 0.9.x (socket protocol 22)**, Git, and a Rust toolchain with `cargo`; Linux
and macOS are supported. Any 0.9 patch release, or a `-preview.*` build of one, is accepted. The board-side compatibility contract remains board protocol v1 and
SQLite schema v16. See the README for the one-line install command itself.

| Component | Required support level | How to verify |
|---|---|---|
| Herdr binary | 0.9.x | `herdr --version` → `herdr 0.9.<patch>` (for example `herdr 0.9.3`) |
 | Herdr socket | protocol 22 | `herdr api schema --json` → top-level `protocol: 22`; a running session's `herdr api snapshot` also reports `version` and `protocol` |
| Board socket | v1 | `docs/protocol.md` and `board-core::protocol` |
| SQLite | schema v16 | `schema.sql` and `board-core::db` migrations |
| Pi integration | v8 for precise Pi lifecycle/session signals | `herdr integration status` |
| Claude integration | v7 for precise Claude lifecycle/session signals | `herdr integration status` |
| Antigravity CLI integration | v1 for the `agy` conversation-id capture (resume/retry/rescue) | `herdr integration status` |

The board rejects a Herdr version outside the 0.9 series, or a different socket protocol, before workspace discovery or pane
placement; it does not silently fall back to an older wire contract. The integration versions are
user-managed prerequisites, not plugin files installed by herdr-board.

## Verify the installed Herdr before installing

These are read-only checks against the binary and session you are about to use:

```bash
herdr --version | grep -Eq '^herdr 0\.9\.(0|[1-9][0-9]*)(-preview\..*)?$'
herdr api schema --json | python3 -c \
  'import json, sys; s=json.load(sys.stdin); assert s["protocol"] == 22, s'
herdr api snapshot
herdr integration status
```

Use `herdr api schema --output PATH` when you need a saved schema for review. Confirm that the
status output shows Pi **current (v8)** and Claude **current (v7)** before relying on precise
working/blocked/done or session-identity signals. Antigravity users: confirm the status shows
**antigravity-cli current (v1)** before relying on conversation reuse — without it an antigravity
run still executes, but its conversation id is never captured. `herdr integration install --help` is the
source of truth for the installable target names; install only the harness integrations you use.

## Installation details and a custom CLI directory

Herdr 0.9.0 first shows an interactive trust preview of the plugin's build commands. Relative
plugin commands resolve from the plugin root, so the manifest's build/action paths do not depend on
the caller's current directory. After approval Herdr checks out the source, builds the release
binary, registers the plugin, and copies the CLI to `~/.local/bin/board` as a regular executable.
After reviewing the manifest and scripts, a noninteractive install is available:

```bash
herdr plugin install nelsonPires5/herdr-board --ref v0.18.0 --yes
```

Set `HERDR_BOARD_CLI_INSTALL_DIR` to an absolute user bin directory before installing to override
`~/.local/bin`; the installed command is `<that-directory>/board`. The installer records the
binary's SHA-256 checksum in `<that-directory>/.herdr-board-cli-managed`. Updates only overwrite a
regular, non-symlink `board` whose contents still match that marker.

## Add a Herdr keybinding

Plugin installation deliberately does not edit `~/.config/herdr/config.toml`. Add a command such as
this yourself (do not reuse a Herdr default; `prefix+k` is `focus_pane_up`, so check
`herdr --default-config`):

```toml
[[keys.command]]
key = "prefix+shift+k"
type = "shell"
command = "herdr plugin action invoke open-board --plugin herdr-board"
```

## Install the harness integration and optional agent skill

For precise Pi status (`idle`, `working`, `blocked`, `done`) and session references, install Herdr's
**Pi v8** integration. Installation changes your personal Pi extension config, so herdr-board never
does it automatically — it is a user prerequisite. Without it (degraded mode), spawn, explicit
`board done`, timeout, and pane-exit handling still work, but Herdr's `working`/`blocked`/`done`
signals do not exist and a card can only reach `awaiting` (pending review) via the idle grace path.

```bash
herdr integration install pi
```

Claude users can similarly run `herdr integration install claude` (the supported Claude integration
is v7). Antigravity users run `herdr integration install antigravity-cli` (the Antigravity CLI
integration, current v1): it installs the hook that reports the `agy` conversation id, which the
daemon captures after launch so later stages, `card run focus`, and rescues can re-attach with
`--conversation <id>`; when the recorded conversation no longer exists the CLI starts a new one and
the daemon persists the new id with a visible card warning. Without the integration the run still
dispatches and `board done` still works, but the mint completes with no recorded conversation id (a
`system` card warning explains the missing integration) and reuse/rescue fail closed — see
[`herdr.md`](herdr.md) for the integration contract. The repository's optional
[`skill/SKILL.md`](../skill/SKILL.md) teaches interactive or dispatched agents to comment, call
`board done`, and queue work. GitHub plugin installation does not copy the skill; the
local-development installer below can do so.

## Linear mode and agent tools

Linear mode needs three things: a Linear API key the daemon can read, the `board mcp` server for
your agents, and a hook that tells the board about Linear writes. Agents create and update
tickets through Linear's own MCP server; the board never writes Linear.

1. Store your Linear API key in the macOS Keychain, where the daemon looks first:

   ```bash
   security add-generic-password -a linear-api-key -s work-linear -w
   ```

   Without the Keychain item the daemon uses `LINEAR_API_KEY` from its own startup environment,
   then `~/.secrets`. The work plugin uses the same item, so a plugin user already has it.

2. Add the board's MCP server for every Claude Code session, once:

   ```bash
   claude mcp add --scope user board -- board mcp
   ```

   The server is the `board` binary itself. It forwards each tool call to the daemon and starts
   the daemon when it is not running. The tools are listed in the skill's "Agent tools" section
   (`board skill`).

3. Report Linear writes to the board. The work plugin ships this hook. Without the plugin, add it
   to your Claude Code `settings.json`:

   ```json
   {
     "hooks": {
       "PostToolUse": [
         {
           "matcher": "mcp__.*[Ll][Ii][Nn][Ee][Aa][Rr].*__.*",
           "hooks": [{ "type": "command", "command": "board linear report" }]
         }
       ]
     }
   }
   ```

   The matcher catches every MCP server whose name contains "linear", whatever your install calls
   it. `board linear report` reads the hook's JSON on stdin, ignores reads, records each write,
   and refreshes the open boards for that space. When an agent in a known, unbound session saves
   an issue, the board links that session's worktree to it; otherwise the board puts a suggestion
   mark on the card for you to accept. The command prints nothing and always exits 0, so a board
   problem never blocks the agent.

4. Optional: show the agent's bound issue in Claude Code's status line with
   `{"statusLine": {"type": "command", "command": "board linear status-line"}}`.

### Moving from the work plugin

The board keeps its own copy of the work plugin's bindings and grouping config.
`board import work-store` copies them from `~/.claude/work` (or `HERDR_LINEAR_STORE_DIR` in the
daemon's environment). It only reads the store, and it never overwrites a row the board already
holds, so you can run it as often as you like. `--dry-run` lists what it would import and skip
without writing anything.

Switch over in this order, so no write is lost:

1. Upgrade the board, then run `board import work-store` straight away. Until you do, an unbound
   space says the store was never imported and names the command.
2. Update the work plugin to the release whose skills and hooks call `board`. That release stops
   writing `~/.claude/work` and stops running its own herdr sync scripts.
3. End or restart running Claude sessions. A session keeps the old plugin's hooks until it
   restarts.
4. Run `board import work-store` again. It adds only what the old plugin wrote in between.

After the second import, no file in `~/.claude/work` should be newer than the import.

## Use named Herdr sessions

Herdr keeps a plugin registry per session, while keybindings/configuration are global. Run the
GitHub install command once from every named session where the plugin should be registered.

A single board daemon serves every scoped board across every Herdr session. Each card carries a
`session` (the default session when unset), and dispatch resolves that session's socket through
`herdr session list`. Use `BOARD_SOCKET` and `BOARD_DB` overrides only when you want a completely
separate board stack.
