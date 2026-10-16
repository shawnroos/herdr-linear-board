#!/usr/bin/env bash
# 46-board-levels.sh — the Linear board picks its level from where its pane sits. A board the daemon
# opens in its own tab shows the space board; a pane split beside it in a worktree bound to TEAM-46
# turns it into that issue's view; closing the split turns it back. A board opened as an overlay over
# a bound pane shows the space board even with focus on that pane beneath it.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_init
e2e_build
e2e_isolate
STORE="$E2E_TMP/work-store"
mkdir -m 700 -p "$STORE/workspaces"
export HERDR_LINEAR_STORE_DIR="$STORE"
# A loopback port nothing listens on keeps every Linear read local: the space board shows its read
# error, and the split level shows the issue from the binding alone.
export BOARD_LINEAR_API_URL=http://127.0.0.1:9/graphql
e2e_daemon_start

step "HERDR MUTATION: link the herdr-board plugin (this checkout) into the ephemeral session"
e2e_herdr_mutate -- plugin link "$REPO_ROOT" >/dev/null

e2e_ws_standard board-levels

panes_in() {  # panes_in <workspace> — pane_id<TAB>tab_id per line
  hrpc pane.list "{\"workspace_id\":\"$1\"}" | python3 -c '
import json,sys
for p in json.load(sys.stdin).get("panes", []):
    print(p["pane_id"] + "\t" + p["tab_id"])'
}
screen_of() {  # screen_of <pane> — the pane's visible text
  "$HERDR_BIN" pane read "$1" --source visible --lines 200 2>/dev/null || true
}
wait_shows() {  # wait_shows <pane> <text> — until the pane shows text, or fail
  local screen=""
  for _ in $(seq 1 150); do
    screen="$(screen_of "$1")"
    printf '%s' "$screen" | grep -q -- "$2" && return 0
    sleep 0.1
  done
  fail "pane $1 never showed '$2' -- got: $screen"
}
wait_drawn_without() {  # wait_drawn_without <pane> <text> — drawn, then 3s without text
  local screen=""
  for _ in $(seq 1 150); do
    screen="$(screen_of "$1")"
    [ -n "$(printf '%s' "$screen" | tr -d '[:space:]')" ] && break
    sleep 0.1
  done
  [ -n "$(printf '%s' "$screen" | tr -d '[:space:]')" ] || fail "pane $1 never drew anything"
  for _ in $(seq 1 30); do
    screen="$(screen_of "$1")"
    printf '%s' "$screen" | grep -q -- "$2" && fail "pane $1 shows '$2' -- got: $screen"
    sleep 0.1
  done
}

AGENT_PANE="" AGENT_TAB=""
for _ in $(seq 1 50); do
  read -r AGENT_PANE AGENT_TAB < <(panes_in "$WS_ID" | head -n1) || true
  [ -n "$AGENT_PANE" ] && break
  sleep 0.1
done
[ -n "$AGENT_PANE" ] || fail "workspace $WS_ID never listed its root pane"

step "Bind the space through the work-store import and a worktree to TEAM-46"
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
brpc linear.bind "{\"cwd\":\"$WORKTREE\",\"issue\":\"TEAM-46\",\"space\":\"$WS_ID\"}" >/dev/null
ok "space $WS_ID bound in $E2E_SESSION; worktree $WORKTREE -> TEAM-46"

step "HERDR MUTATION: focus the disposable workspace"
e2e_herdr_mutate -- workspace focus "$WS_ID" >/dev/null

step "HERDR MUTATION: start board mcp as the agent pane would, and open a board in its own tab"
coproc MCP {
  export HERDR_PANE_ID="$AGENT_PANE" HERDR_WORKSPACE_ID="$WS_ID"
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
reply_field() {  # reply_field <python expression over r = the JSON-RPC reply>
  python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$MCP_REPLY" "$1"
}
mcp_request 1 initialize '{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e-46","version":"0"}}'
mcp_send '{"jsonrpc":"2.0","method":"notifications/initialized"}'
mcp_request 2 tools/call "{\"name\":\"open_board\",\"arguments\":{\"placement\":\"tab\",\"space\":\"$WS_ID\"}}"
[ "$(reply_field 'bool(r["result"].get("isError"))')" = False ] || fail "open_board tab failed: $(cat "$MCP_REPLY")"
BOARD_PANE="$(reply_field 'r["result"]["structuredContent"]["pane_id"]')"
BOARD_TAB="$(reply_field 'r["result"]["structuredContent"]["tab_id"]')"
[ "$BOARD_TAB" != "$AGENT_TAB" ] || fail "the board did not get a tab of its own: $(cat "$MCP_REPLY")"
ok "board pane $BOARD_PANE alone in tab $BOARD_TAB"

step "Alone in its tab, the board shows the space board"
wait_drawn_without "$BOARD_PANE" 'TEAM-46'
ok "space level: no issue view in $BOARD_PANE"

step "HERDR MUTATION: split a focused pane beside the board in the TEAM-46 worktree"
SPLIT_PANE="$(e2e_hrpc_mutate -- pane.split "{\"target_pane_id\":\"$BOARD_PANE\",\"direction\":\"right\",\"focus\":true,\"cwd\":\"$WORKTREE/src\"}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin); print((r.get("pane") or r)["pane_id"])')"
[ -n "$SPLIT_PANE" ] || fail "pane.split returned no pane"
wait_shows "$BOARD_PANE" 'TEAM-46'
ok "split level: $BOARD_PANE shows TEAM-46 beside $SPLIT_PANE"

step "HERDR MUTATION: close the split; the board is alone again"
e2e_hrpc_mutate -- pane.close "{\"pane_id\":\"$SPLIT_PANE\"}" >/dev/null
gone=""
for _ in $(seq 1 150); do
  printf '%s' "$(screen_of "$BOARD_PANE")" | grep -q 'TEAM-46' || { gone=1; break; }
  sleep 0.1
done
[ -n "$gone" ] || fail "board pane $BOARD_PANE still shows TEAM-46 after the split closed -- got: $(screen_of "$BOARD_PANE")"
ok "space level again in $BOARD_PANE"

step "HERDR MUTATION: open the board as an overlay over a bound pane, then focus that pane"
e2e_hrpc_mutate -- pane.focus "{\"pane_id\":\"$AGENT_PANE\"}" >/dev/null
BOUND_PANE="$(e2e_hrpc_mutate -- pane.split "{\"target_pane_id\":\"$AGENT_PANE\",\"direction\":\"right\",\"focus\":true,\"cwd\":\"$WORKTREE/src\"}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin); print((r.get("pane") or r)["pane_id"])')"
OVERLAY_PANE="$(e2e_hrpc_mutate -- plugin.pane.open "$(python3 -c '
import json,sys
print(json.dumps({"plugin_id":"herdr-board","entrypoint":"board","placement":"overlay","focus":True,
                  "env":{"BOARD_SOCKET":sys.argv[1],"BOARD_DB":sys.argv[2]}}))' "$BOARD_SOCKET" "$BOARD_DB")" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["plugin_pane"]["pane"]["pane_id"])')"
[ -n "$OVERLAY_PANE" ] || fail "plugin.pane.open returned no overlay pane"
wait_drawn_without "$OVERLAY_PANE" 'TEAM-46'
e2e_hrpc_mutate -- pane.focus "{\"pane_id\":\"$BOUND_PANE\"}" >/dev/null
wait_drawn_without "$OVERLAY_PANE" 'TEAM-46'
ok "the overlay $OVERLAY_PANE shows the space board with focus on bound pane $BOUND_PANE"

exec {MCP_IN}>&-
wait "$MCP_PID" 2>/dev/null || true

echo; echo "46-board-levels: PASS"
