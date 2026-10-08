#!/usr/bin/env bash
# 46-caller-without-herdr-env.sh — a Claude session that lost herdr's variables (Claude Code's
# warm-spare claim) still finds its pane: `board mcp` with no HERDR_* env refuses open_board and
# names the one Claude pane in its folder, opens the board beside it once the caller passes that
# `<session>/<pane id>`, and a separate `board caller` with the same Claude session id reads the
# remembered pane from boardd. A second Claude pane in the folder makes a fresh session id
# unconfirmed with both candidates; the lookup never picks one on its own.
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

# The daemon compares canonical paths, and /tmp can be a symlink.
D="$E2E_TMP/project"
mkdir -p "$D"
D="$(cd "$D" && pwd -P)"

step "HERDR MUTATION: create disposable workspace whose root pane runs in $D"
ws_json="$(e2e_herdr_mutate -- workspace create --cwd "$D" --label caller-env --no-focus \
  --env "BOARD_BIN=$BOARD_BIN" --env "BOARD_SOCKET=$BOARD_SOCKET" \
  --env "BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH")"
WS_ID="$(printf '%s' "$ws_json" | jget workspace_id)"
e2e_ws_defer_close "$WS_ID"
echo "  workspace: $WS_ID"

panes_in() {  # panes_in <workspace> — pane_id<TAB>tab_id per line
  hrpc pane.list "{\"workspace_id\":\"$1\"}" | python3 -c '
import json,sys
for p in json.load(sys.stdin).get("panes", []):
    print(p["pane_id"] + "\t" + p["tab_id"])'
}
focused_pane() {
  hrpc session.snapshot '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["snapshot"].get("focused_pane_id") or "")'
}
clock_ms() { python3 -c 'import time; print(time.time_ns() // 1_000_000)'; }

AGENT_PANE="" AGENT_TAB=""
for _ in $(seq 1 50); do
  read -r AGENT_PANE AGENT_TAB < <(panes_in "$WS_ID" | head -n1) || true
  [ -n "$AGENT_PANE" ] && break
  sleep 0.1
done
[ -n "$AGENT_PANE" ] || fail "workspace $WS_ID never listed its root pane"

# Herdr orders integration reports by source sequence; nanosecond scale keeps a report from
# reading as stale against an earlier one on the same pane.
SEQ="$(python3 -c 'import time; print(time.time_ns())')"
# herdr 0.9.0 lists no agent for a bare shell pane even after `pane report-agent`, so a stand-in
# binary named `claude` (a copy of sleep: no provider, no Claude Code) runs in the pane first.
FAKE_CLAUDE="$E2E_TMP/bin/claude"
mkdir -p "$E2E_TMP/bin"
install -m 755 "$(command -v sleep)" "$FAKE_CLAUDE"
report_claude() {  # report_claude <pane> — the pane runs `claude` and reports it, as Claude's herdr hook does
  e2e_herdr_mutate -- pane run "$1" "$FAKE_CLAUDE 600" >/dev/null
  SEQ=$((SEQ + 1))
  e2e_herdr_mutate -- pane report-agent "$1" --source herdr:claude --agent claude \
    --agent-session-path "$E2E_TMP/claude-$1.jsonl" --state idle --seq "$SEQ" >/dev/null
}
assert_claude_in_d() {  # assert_claude_in_d <pane> — pane.list shows agent claude and cwd D
  local got=""
  for _ in $(seq 1 100); do
    got="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json,sys
pane,cwd=sys.argv[1:]
for p in json.load(sys.stdin).get("panes", []):
    if p["pane_id"]==pane:
        print("yes" if p.get("agent")=="claude" and cwd in (p.get("cwd"), p.get("foreground_cwd")) else json.dumps(p))
' "$1" "$D")"
    [ "$got" = yes ] && return 0
    sleep 0.1
  done
  fail "pane $1 does not list as a claude pane in $D: $got"
}

step "HERDR MUTATION: the root pane reports agent claude"
report_claude "$AGENT_PANE"
assert_claude_in_d "$AGENT_PANE"
ok "pane $AGENT_PANE in tab $AGENT_TAB lists agent claude with cwd $D"

step "HERDR MUTATION: focus the disposable workspace so the agent pane holds focus"
e2e_herdr_mutate -- workspace focus "$WS_ID" >/dev/null
[ "$(focused_pane)" = "$AGENT_PANE" ] || fail "the agent pane $AGENT_PANE is not focused before the open"
ok "agent pane $AGENT_PANE holds focus"

# What a claimed warm spare sees: none of herdr's variables. HERDR_BOARD_CONFIG and
# HERDR_LINEAR_STORE_DIR are the board's own settings, not herdr's, so they stay.
herdr_env_unset_args() {
  local v
  for v in $(compgen -e); do
    case "$v" in
      HERDR_BOARD_CONFIG|HERDR_LINEAR_STORE_DIR) ;;
      HERDR_*) printf -- '-u\n%s\n' "$v" ;;
    esac
  done
}
mapfile -t NO_HERDR_ENV < <(herdr_env_unset_args)
caller_json() {  # caller_json <claude session id> [board caller args...] — a separate process in D
  local session="$1"
  shift
  (cd "$D" && env "${NO_HERDR_ENV[@]}" CLAUDE_CODE_SESSION_ID="$session" "$BOARD_BIN" caller --json "$@")
}

step "HERDR MUTATION: start board mcp in $D with no herdr env (identity gated; every open goes through boardd)"
# rmcp ends the session at stdin EOF and can drop an in-flight call, so requests go one at a time
# over a coprocess and each reply is read before the next request is sent.
coproc MCP {
  cd "$D"
  for v in $(compgen -e); do
    case "$v" in HERDR_BOARD_CONFIG|HERDR_LINEAR_STORE_DIR) ;; HERDR_*) unset "$v" ;; esac
  done
  export CLAUDE_CODE_SESSION_ID=e2e-46-a
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

mcp_request 1 initialize '{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e-46","version":"0"}}'
[ "$(reply_field 'r["result"]["serverInfo"]["name"]')" = board ] || fail "board mcp did not initialize: $(cat "$MCP_REPLY")"
mcp_send '{"jsonrpc":"2.0","method":"notifications/initialized"}'
ok "board mcp initialized"

CANDIDATE="$E2E_SESSION/$AGENT_PANE"

step "open_board without pane refuses and names the one Claude pane in $D"
call_tool 2 open_board "{\"placement\":\"split\",\"space\":\"$WS_ID\"}"
[ "$(reply_field 'bool(r["result"].get("isError"))')" = True ] || fail "open_board without pane did not refuse: $(cat "$MCP_REPLY")"
[ "$(reply_field 'r["result"]["structuredContent"]["state"]')" = unconfirmed ] \
  || fail "the refusal is not an unconfirmed result: $(cat "$MCP_REPLY")"
[ "$(reply_field '[c["pane"] for c in r["result"]["structuredContent"]["candidates"]]')" = "['$CANDIDATE']" ] \
  || fail "the refusal does not name exactly $CANDIDATE: $(cat "$MCP_REPLY")"
reply_field 'r["result"]["content"][0]["text"]' | grep -Fq "$CANDIDATE" \
  || fail "the refusal text does not name $CANDIDATE: $(cat "$MCP_REPLY")"
reply_field 'r["result"]["content"][0]["text"]' | grep -Fq 'Ask the person' \
  || fail "the refusal text does not tell the agent to ask the person: $(cat "$MCP_REPLY")"
[ "$(panes_in "$WS_ID" | wc -l)" -eq 1 ] || fail "the refused open still changed the panes"
ok "refused as unconfirmed with the one candidate $CANDIDATE; nothing opened"

step "open_board with pane $CANDIDATE opens the board beside it, unfocused"
call_tool 3 open_board "{\"placement\":\"split\",\"space\":\"$WS_ID\",\"pane\":\"$CANDIDATE\"}"
[ "$(reply_field 'bool(r["result"].get("isError"))')" = False ] || fail "open_board with pane failed: $(cat "$MCP_REPLY")"
BOARD_PANE="$(reply_field 'r["result"]["structuredContent"]["pane_id"]')"
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

step "A separate board caller with the same Claude session id reads the pane boardd remembered"
caller_json e2e-46-a >"$E2E_TMP/caller-a.json" || fail "board caller failed: $(cat "$E2E_TMP/caller-a.json")"
python3 - "$E2E_TMP/caller-a.json" "$E2E_SESSION" "$AGENT_PANE" "$WS_ID" <<'PY' \
  || fail "board caller did not resolve to $CANDIDATE: $(cat "$E2E_TMP/caller-a.json")"
import json,sys
path,session,pane,ws=sys.argv[1:]
r=json.load(open(path))
loc=r.get("location") or {}
raise SystemExit(0 if r.get("state")=="resolved" and loc.get("session")==session
                 and loc.get("pane_id")==pane and loc.get("workspace_id")==ws else 1)
PY
ok "e2e-46-a resolves to $CANDIDATE in a process that never saw the confirmation"

step "HERDR MUTATION: add a second Claude pane in $D"
tab_json="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --cwd "$D" --label caller-env-2 --no-focus)"
SECOND_PANE="$(printf '%s' "$tab_json" | jget pane_id)"
[ -n "$SECOND_PANE" ] || fail "the second pane was not created: $tab_json"
report_claude "$SECOND_PANE"
assert_claude_in_d "$SECOND_PANE"
ok "pane $SECOND_PANE lists agent claude with cwd $D"

step "A fresh Claude session id is unconfirmed with both candidates (timed cross-session lookup)"
started="$(clock_ms)"
caller_json e2e-46-b >"$E2E_TMP/caller-b.json" || fail "board caller failed: $(cat "$E2E_TMP/caller-b.json")"
LOOKUP_MS=$(( $(clock_ms) - started ))
python3 - "$E2E_TMP/caller-b.json" "$E2E_SESSION/$AGENT_PANE" "$E2E_SESSION/$SECOND_PANE" <<'PY' \
  || fail "e2e-46-b is not unconfirmed with both panes: $(cat "$E2E_TMP/caller-b.json")"
import json,sys
path,*want=sys.argv[1:]
r=json.load(open(path))
raise SystemExit(0 if r.get("state")=="unconfirmed"
                 and sorted(c["pane"] for c in r.get("candidates",[]))==sorted(want) else 1)
PY
echo "  lookup: board caller --json (cross-session folder lookup) took ${LOOKUP_MS} ms"
ok "e2e-46-b: unconfirmed, candidates $E2E_SESSION/$AGENT_PANE and $E2E_SESSION/$SECOND_PANE"

step "Naming one of them resolves to that pane only"
caller_json e2e-46-b --pane "$E2E_SESSION/$SECOND_PANE" >"$E2E_TMP/caller-b2.json" \
  || fail "board caller --pane failed: $(cat "$E2E_TMP/caller-b2.json")"
python3 - "$E2E_TMP/caller-b2.json" "$SECOND_PANE" <<'PY' \
  || fail "board caller --pane did not resolve to $SECOND_PANE: $(cat "$E2E_TMP/caller-b2.json")"
import json,sys
r=json.load(open(sys.argv[1]))
raise SystemExit(0 if r.get("state")=="resolved" and (r.get("location") or {}).get("pane_id")==sys.argv[2] else 1)
PY
ok "e2e-46-b --pane $E2E_SESSION/$SECOND_PANE resolves to $SECOND_PANE"

exec {MCP_IN}>&-
wait "$MCP_PID" 2>/dev/null || true

echo; echo "46-caller-without-herdr-env: PASS"
