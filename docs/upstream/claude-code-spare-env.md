# A session that claims a warm spare loses the terminal's HERDR_* variables

**Claude Code version:** 2.1.294 (macOS, arm64)
**Terminal multiplexer:** herdr 0.9.3

## What happens

1. herdr starts each pane's shell with `HERDR_PANE_ID`, `HERDR_WORKSPACE_ID` and `HERDR_TAB_ID`.
2. Run `claude` in a new herdr pane. It claims a warm spare (`claude bg-spare`) that the daemon started earlier.
3. In that session, the Bash tool does not see any `HERDR_*` variable. MCP servers started after the claim also do not see them.

A plain shell in the same herdr tab prints:

```
HERDR_ENV=1
HERDR_PANE_ID=w2:p2
HERDR_TAB_ID=w2:t1
HERDR_WORKSPACE_ID=w2
```

Inside the session, `env | grep ^HERDR_` printed only one variable, `HERDR_LINEAR_SLATE_ROOT`. That variable comes from the user's shell profile.

## Process tree

```
herdr server
 └─ zsh (pane shell, started 09:03:15)        has HERDR_*
     └─ claude (started 09:03:45)             front end
claude daemon
 └─ claude bg-pty-host
     └─ claude bg-spare (started 08:56:19)    runs tools and MCP servers: no HERDR_*
         └─ board mcp (started 09:03:47)      no HERDR_*
```

A session that does not claim a spare (`claude --continue` in another pane) has the variables.

## Expected

Tools and MCP servers in a claimed session see the same terminal-identity variables as the terminal that started `claude`. For tmux, this already happens: `TMUX_PANE` is in Claude Code's list of terminal variables. herdr's variables are not on that list.

## Impact

Tools that find their pane through these variables cannot work in such a session. The herdr board cannot open a split beside the session or know which herdr workspace it is in. Its hooks cannot report which pane a Linear write came from.

## Suggested fix

Forward `HERDR_*` (or every variable of the claiming terminal that the spare's own environment does not set) when a front end claims a spare.
