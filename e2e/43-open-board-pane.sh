#!/usr/bin/env bash
# 43-open-board-pane.sh — an agent asks `board mcp` to open a board beside its own pane: the
# herdr-board plugin (this checkout, linked into the ephemeral session) opens as a split in the
# agent's tab, wired to the scenario's own boardd and showing the requested space, without taking
# focus; a second open for the same context reuses that pane, an overlay is refused, and
# close_board closes it.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_init
e2e_build
e2e_isolate
# The board pane's TUI reads Linear through this daemon. A loopback port nothing listens on keeps
# every Linear read local even where a real credential is reachable.
export BOARD_LINEAR_API_URL=http://127.0.0.1:9/graphql
e2e_daemon_start

step "HERDR MUTATION: link the herdr-board plugin (this checkout) into the ephemeral session"
e2e_herdr_mutate -- plugin link "$REPO_ROOT" >/dev/null

e2e_ws_standard open-board

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

step "HERDR MUTATION: focus the disposable workspace so the agent pane holds focus"
e2e_herdr_mutate -- workspace focus "$WS_ID" >/dev/null
[ "$(focused_pane)" = "$AGENT_PANE" ] || fail "the agent pane $AGENT_PANE is not focused before the open"
ok "agent pane $AGENT_PANE in tab $AGENT_TAB holds focus"

step "HERDR MUTATION: start board mcp as the agent pane would (identity gated; every open goes through boardd)"
# rmcp ends the session at stdin EOF and can drop an in-flight call, so requests go one at a time
# over a coprocess and each reply is read before the next request is sent.
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
call_tool() {  # call_tool <id> <name> <arguments-json>
  mcp_request "$1" tools/call "{\"name\":\"$2\",\"arguments\":$3}"
}
reply_field() {  # reply_field <python expression over r = the JSON-RPC reply>
  python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$MCP_REPLY" "$1"
}

mcp_request 1 initialize '{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e-43","version":"0"}}'
[ "$(reply_field 'r["result"]["serverInfo"]["name"]')" = board ] || fail "board mcp did not initialize: $(cat "$MCP_REPLY")"
mcp_send '{"jsonrpc":"2.0","method":"notifications/initialized"}'
ok "board mcp initialized"

step "open_board with split opens the board beside the agent pane, unfocused"
call_tool 2 open_board "{\"placement\":\"split\",\"space\":\"$WS_ID\"}"
[ "$(reply_field 'bool(r["result"].get("isError"))')" = False ] || fail "open_board split failed: $(cat "$MCP_REPLY")"
BOARD_PANE="$(reply_field 'r["result"]["structuredContent"]["pane_id"]')"
[ "$(reply_field 'r["result"]["structuredContent"]["reused"]')" = False ] || fail "the first open claimed reuse: $(cat "$MCP_REPLY")"
[ "$(reply_field 'r["result"]["structuredContent"]["tab_id"]')" = "$AGENT_TAB" ] \
  || fail "the board did not open in the agent's tab: $(cat "$MCP_REPLY")"
[ "$BOARD_PANE" != "$AGENT_PANE" ] || fail "open_board answered with the agent's own pane"
listed=""
for _ in $(seq 1 50); do
  listed="$(panes_in "$WS_ID" | awk -F'\t' -v p="$BOARD_PANE" '$1==p {print $2}')"
  [ -n "$listed" ] && break
  sleep 0.1
done
[ "$listed" = "$AGENT_TAB" ] || fail "board pane $BOARD_PANE is not listed in tab $AGENT_TAB (got '$listed')"
[ "$(panes_in "$WS_ID" | awk -F'\t' -v t="$AGENT_TAB" '$2==t' | wc -l)" -eq 2 ] \
  || fail "the agent tab should hold exactly the agent pane and the board"
[ "$(focused_pane)" = "$AGENT_PANE" ] || fail "focus moved off the agent pane to $(focused_pane)"
ok "board pane $BOARD_PANE split into $AGENT_TAB; focus stayed on $AGENT_PANE"

step "The board pane runs against this scenario's boardd and shows the requested space"
if [ -d /proc/self ]; then
  wired=""
  for _ in $(seq 1 100); do
    wired="$(python3 - "$BOARD_PANE" "$BOARD_SOCKET" "$WS_ID" <<'PY'
import os,sys
pane,socket,space=sys.argv[1:]
for pid in filter(str.isdigit, os.listdir("/proc")):
    try:
        raw=open(f"/proc/{pid}/environ","rb").read()
    except OSError:
        continue
    env=dict(item.split(b"=",1) for item in raw.split(b"\0") if b"=" in item)
    if env.get(b"HERDR_PANE_ID")!=pane.encode() or b"BOARD_SHOW_SPACE" not in env:
        continue
    ok=(os.path.realpath(env.get(b"BOARD_SOCKET",b"").decode())==os.path.realpath(socket)
        and env[b"BOARD_SHOW_SPACE"].decode()==space)
    print("yes" if ok else "mismatch")
    raise SystemExit(0)
print("")
PY
)"
    [ -n "$wired" ] && break
    sleep 0.1
  done
  [ "$wired" = yes ] || fail "no process in board pane $BOARD_PANE carries this boardd's socket and BOARD_SHOW_SPACE=$WS_ID (got '$wired')"
  ok "the board pane's process has BOARD_SOCKET=$BOARD_SOCKET and BOARD_SHOW_SPACE=$WS_ID"
else
  echo "  skipped: no /proc on this host, so the board pane's environment cannot be read"
fi

step "A second open for the same context reuses the recorded pane"
call_tool 3 open_board "{\"placement\":\"split\",\"space\":\"$WS_ID\"}"
[ "$(reply_field 'r["result"]["structuredContent"]["pane_id"]')" = "$BOARD_PANE" ] \
  || fail "the second open did not return $BOARD_PANE: $(cat "$MCP_REPLY")"
[ "$(reply_field 'r["result"]["structuredContent"]["reused"]')" = True ] || fail "the second open did not report reuse"
[ "$(panes_in "$WS_ID" | wc -l)" -eq 2 ] || fail "the second open created another pane"
[ "$(focused_pane)" = "$AGENT_PANE" ] || fail "the second open moved focus"
ok "same pane $BOARD_PANE, reused"

step "An overlay is refused as a tool error before herdr is asked"
call_tool 4 open_board "{\"placement\":\"overlay\",\"space\":\"$WS_ID\"}"
[ "$(reply_field 'bool(r["result"].get("isError"))')" = True ] || fail "overlay was not refused: $(cat "$MCP_REPLY")"
[ "$(reply_field 'r["result"]["structuredContent"]["code"]')" = 1 ] || fail "overlay refusal was not code 1: $(cat "$MCP_REPLY")"
[ "$(panes_in "$WS_ID" | wc -l)" -eq 2 ] || fail "the refused overlay still changed the panes"
ok "overlay -> code 1, nothing opened"

step "close_board closes the board pane it opened"
call_tool 5 close_board "{\"space\":\"$WS_ID\"}"
[ "$(reply_field 'r["result"]["structuredContent"]["pane_id"]')" = "$BOARD_PANE" ] || fail "close named another pane: $(cat "$MCP_REPLY")"
[ "$(reply_field 'r["result"]["structuredContent"]["closed"]')" = True ] || fail "close did not close: $(cat "$MCP_REPLY")"
gone=""
for _ in $(seq 1 50); do
  panes_in "$WS_ID" | awk -F'\t' -v p="$BOARD_PANE" '$1==p {found=1} END {exit found ? 1 : 0}' && { gone=1; break; }
  sleep 0.1
done
[ -n "$gone" ] || fail "board pane $BOARD_PANE is still listed after close_board"
[ "$(focused_pane)" = "$AGENT_PANE" ] || fail "close moved focus off the agent pane"
ok "board pane closed; the agent pane keeps focus"

exec {MCP_IN}>&-
wait "$MCP_PID" 2>/dev/null || true

echo; echo "43-open-board-pane: PASS"
