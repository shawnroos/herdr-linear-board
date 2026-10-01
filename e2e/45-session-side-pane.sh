#!/usr/bin/env bash
# 45-session-side-pane.sh — an agent whose worktree is bound to an issue asks `board mcp` to open
# its session side pane: the herdr-board plugin's `session` entrypoint (this checkout, linked into
# the ephemeral session) opens as a split in the agent's tab, carries the agent's identity in its
# environment, shows the bound issue, and never takes focus; close_board with session closes it.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_init
e2e_build
e2e_isolate
# The space binding is seeded through the work-store import, as in 44.
STORE="$E2E_TMP/work-store"
mkdir -m 700 -p "$STORE/workspaces"
export HERDR_LINEAR_STORE_DIR="$STORE"
# A loopback port nothing listens on keeps every Linear read local: the pane shows the bound issue
# from the session read alone.
export BOARD_LINEAR_API_URL=http://127.0.0.1:9/graphql
e2e_daemon_start

step "HERDR MUTATION: link the herdr-board plugin (this checkout) into the ephemeral session"
e2e_herdr_mutate -- plugin link "$REPO_ROOT" >/dev/null

e2e_ws_standard session-pane

panes_in() {  # panes_in <workspace> — pane_id<TAB>tab_id per line
  hrpc pane.list "{\"workspace_id\":\"$1\"}" | python3 -c '
import json,sys
for p in json.load(sys.stdin).get("panes", []):
    print(p["pane_id"] + "\t" + p["tab_id"])'
}
focused_pane() {
  hrpc session.snapshot '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["snapshot"].get("focused_pane_id") or "")'
}

AGENT_PANE="" AGENT_TAB=""
for _ in $(seq 1 50); do
  read -r AGENT_PANE AGENT_TAB < <(panes_in "$WS_ID" | head -n1) || true
  [ -n "$AGENT_PANE" ] && break
  sleep 0.1
done
[ -n "$AGENT_PANE" ] || fail "workspace $WS_ID never listed its root pane"

step "Bind the space through the work-store import and the agent's worktree to TEAM-45"
mkdir -m 700 -p "$STORE/workspaces/$E2E_SESSION"
python3 - "$STORE/workspaces/$E2E_SESSION/$WS_ID.json" "$WS_ID" <<'PY'
import json,os,sys
path,ws=sys.argv[1:]
with open(path,"w") as f:
    json.dump({"version":1,"worktree_path":f"workspace:{ws}","state":"bound","issue_identifier":"project-e2e"},f)
os.chmod(path,0o600)
PY
"$BOARD_BIN" import work-store --json >"$E2E_TMP/import.json"
WORKTREE="$E2E_TMP/worktree"
mkdir -p "$WORKTREE/.git" "$WORKTREE/src"
WORKTREE="$(cd "$WORKTREE" && pwd -P)"
brpc linear.bind "{\"cwd\":\"$WORKTREE\",\"issue\":\"TEAM-45\",\"space\":\"$WS_ID\"}" >/dev/null
brpc linear.session.get "{\"space\":\"$WS_ID\",\"herdr_socket\":\"$HERDR_SOCKET_PATH\",\"cwd\":\"$WORKTREE/src\"}" \
  >"$E2E_TMP/session.json"
python3 - "$E2E_TMP/session.json" <<'PY' || fail "the agent's session does not read as bound to TEAM-45: $(cat "$E2E_TMP/session.json")"
import json,sys
r=json.load(open(sys.argv[1]))
raise SystemExit(0 if r["space_bound"] and (r.get("binding") or {}).get("issue")=="TEAM-45" else 1)
PY
ok "space $WS_ID bound in $E2E_SESSION; worktree $WORKTREE -> TEAM-45"

step "HERDR MUTATION: focus the disposable workspace so the agent pane holds focus"
e2e_herdr_mutate -- workspace focus "$WS_ID" >/dev/null
[ "$(focused_pane)" = "$AGENT_PANE" ] || fail "the agent pane $AGENT_PANE is not focused before the open"
ok "agent pane $AGENT_PANE in tab $AGENT_TAB holds focus"

step "HERDR MUTATION: start board mcp from the agent's worktree as the agent pane would"
# rmcp ends the session at stdin EOF and can drop an in-flight call, so requests go one at a time
# over a coprocess and each reply is read before the next request is sent.
coproc MCP {
  cd "$WORKTREE/src"
  export HERDR_PANE_ID="$AGENT_PANE" HERDR_WORKSPACE_ID="$WS_ID" CLAUDE_CODE_SESSION_ID=e2e-45
  unset BOARD_CARD_ID BOARD_RUN_ID
  e2e_board_herdr_mutate -- mcp 2>>"$E2E_TMP/mcp.stderr"
}
MCP_IN="${MCP[1]}" MCP_OUT="${MCP[0]}"
MCP_REPLY="$E2E_TMP/mcp-reply.json"

mcp_send() { printf '%s\n' "$1" >&"$MCP_IN"; }
mcp_request() {  # mcp_request <id> <method> <params-json> — the matching reply lands in $MCP_REPLY
  local id="$1" line
  mcp_send "$(python3 -c 'import json,sys; print(json.dumps({"jsonrpc":"2.0","id":int(sys.argv[1]),"method":sys.argv[2],"params":json.loads(sys.argv[3])}))' "$id" "$2" "$3")"
  while IFS= read -r -t 60 line <&"$MCP_OUT"; do
    if printf '%s' "$line" | python3 -c 'import json,sys; raise SystemExit(0 if json.load(sys.stdin).get("id")==int(sys.argv[1]) else 1)' "$id"; then
      printf '%s' "$line" >"$MCP_REPLY"
      return 0
    fi
  done
  fail "board mcp gave no reply to request $id ($2); stderr: $(tail -n 5 "$E2E_TMP/mcp.stderr" 2>/dev/null)"
}
call_tool() {  # call_tool <id> <name> <arguments-json>
  mcp_request "$1" tools/call "{\"name\":\"$2\",\"arguments\":$3}"
}
reply_field() {  # reply_field <python expression over r = the JSON-RPC reply>
  python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$MCP_REPLY" "$1"
}

mcp_request 1 initialize '{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e-45","version":"0"}}'
[ "$(reply_field 'r["result"]["serverInfo"]["name"]')" = board ] || fail "board mcp did not initialize: $(cat "$MCP_REPLY")"
mcp_send '{"jsonrpc":"2.0","method":"notifications/initialized"}'
ok "board mcp initialized"

step "open_board with session opens the side pane beside the agent pane, unfocused"
call_tool 2 open_board '{"session":true}'
[ "$(reply_field 'bool(r["result"].get("isError"))')" = False ] || fail "open_board session failed: $(cat "$MCP_REPLY")"
SESSION_PANE="$(reply_field 'r["result"]["structuredContent"]["pane_id"]')"
[ "$(reply_field 'r["result"]["structuredContent"]["tab_id"]')" = "$AGENT_TAB" ] \
  || fail "the session pane did not open in the agent's tab: $(cat "$MCP_REPLY")"
[ "$SESSION_PANE" != "$AGENT_PANE" ] || fail "open_board answered with the agent's own pane"
listed=""
for _ in $(seq 1 50); do
  listed="$(panes_in "$WS_ID" | awk -F'\t' -v p="$SESSION_PANE" '$1==p {print $2}')"
  [ -n "$listed" ] && break
  sleep 0.1
done
[ "$listed" = "$AGENT_TAB" ] || fail "session pane $SESSION_PANE is not listed in tab $AGENT_TAB (got '$listed')"
[ "$(focused_pane)" = "$AGENT_PANE" ] || fail "focus moved off the agent pane to $(focused_pane)"
ok "session pane $SESSION_PANE split into $AGENT_TAB; focus stayed on $AGENT_PANE"

step "The session pane carries the agent's identity, not its own"
if [ -d /proc/self ]; then
  wired=""
  for _ in $(seq 1 100); do
    wired="$(python3 - "$SESSION_PANE" "$AGENT_PANE" "$WS_ID" "$WORKTREE/src" "$HERDR_SOCKET_PATH" <<'PY'
import os,sys
pane,agent,space,cwd,socket=sys.argv[1:]
for pid in filter(str.isdigit, os.listdir("/proc")):
    try:
        raw=open(f"/proc/{pid}/environ","rb").read()
    except OSError:
        continue
    env=dict(item.split(b"=",1) for item in raw.split(b"\0") if b"=" in item)
    if env.get(b"HERDR_PANE_ID")!=pane.encode() or b"BOARD_SESSION_PANE" not in env:
        continue
    got=lambda k: env.get(k.encode(),b"").decode()
    ok=(got("BOARD_SESSION_PANE")==agent and got("BOARD_SESSION_WORKSPACE")==space
        and got("BOARD_SESSION_CWD")==cwd and got("BOARD_SESSION_SOCKET")==socket
        and got("BOARD_SESSION_CLAUDE")=="e2e-45")
    print("yes" if ok else "mismatch " + repr({k:got(k) for k in ("BOARD_SESSION_PANE","BOARD_SESSION_WORKSPACE","BOARD_SESSION_CWD","BOARD_SESSION_SOCKET","BOARD_SESSION_CLAUDE")}))
    raise SystemExit(0)
print("")
PY
)"
    [ -n "$wired" ] && break
    sleep 0.1
  done
  [ "$wired" = yes ] || fail "no process in session pane $SESSION_PANE carries agent $AGENT_PANE's identity (got '$wired')"
  ok "the session pane's process names agent pane $AGENT_PANE, cwd $WORKTREE/src and the raw socket"
else
  echo "  skipped: no /proc on this host, so the session pane's environment cannot be read"
fi

step "The session pane shows the bound issue"
screen=""
for _ in $(seq 1 100); do
  screen="$("$HERDR_BIN" pane read "$SESSION_PANE" --source visible --lines 200 2>/dev/null || true)"
  printf '%s' "$screen" | grep -q 'TEAM-45' && break
  sleep 0.1
done
printf '%s' "$screen" | grep -q 'TEAM-45' || fail "session pane $SESSION_PANE never showed TEAM-45 -- got: $screen"
printf '%s' "$screen" | grep -q 'not bound' && fail "session pane shows the bind hint for a bound session -- got: $screen"
[ "$(focused_pane)" = "$AGENT_PANE" ] || fail "the session pane took focus after drawing"
ok "TEAM-45 on screen in $SESSION_PANE; focus still on $AGENT_PANE"

step "close_board with session closes the side pane"
call_tool 3 close_board '{"session":true}'
[ "$(reply_field 'r["result"]["structuredContent"]["pane_id"]')" = "$SESSION_PANE" ] || fail "close named another pane: $(cat "$MCP_REPLY")"
[ "$(reply_field 'r["result"]["structuredContent"]["closed"]')" = True ] || fail "close did not close: $(cat "$MCP_REPLY")"
gone=""
for _ in $(seq 1 50); do
  panes_in "$WS_ID" | awk -F'\t' -v p="$SESSION_PANE" '$1==p {found=1} END {exit found ? 1 : 0}' && { gone=1; break; }
  sleep 0.1
done
[ -n "$gone" ] || fail "session pane $SESSION_PANE is still listed after close_board"
[ "$(focused_pane)" = "$AGENT_PANE" ] || fail "close moved focus off the agent pane"
ok "session pane closed; the agent pane keeps focus"

exec {MCP_IN}>&-
wait "$MCP_PID" 2>/dev/null || true

echo; echo "45-session-side-pane: PASS"
