#!/usr/bin/env bash
# 44-linear-report-link.sh — a Claude Code PostToolUse payload for the Linear MCP `save_issue`,
# piped into `board linear report` from a real pane in a bound space, links that session's
# worktree to the saved issue; a second save from the now-bound session leaves a suggestion mark
# instead of rebinding. No Linear call: the issue id comes from the hook payload.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_init
e2e_build
e2e_isolate
# The space binding is seeded through the work-store import, the way an upgrade brings it in.
STORE="$E2E_TMP/work-store"
mkdir -m 700 -p "$STORE/workspaces"
export HERDR_LINEAR_STORE_DIR="$STORE"
# A loopback port nothing listens on: the refresh a report schedules stays local.
export BOARD_LINEAR_API_URL=http://127.0.0.1:9/graphql
e2e_daemon_start

e2e_ws_standard report-link

AGENT_PANE=""
for _ in $(seq 1 50); do
  AGENT_PANE="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json,sys
panes=json.load(sys.stdin).get("panes", [])
print(panes[0]["pane_id"] if panes else "")')"
  [ -n "$AGENT_PANE" ] && break
  sleep 0.1
done
[ -n "$AGENT_PANE" ] || fail "workspace $WS_ID never listed its root pane"

step "Bind the space through the work-store import so the agent pane is a known session"
mkdir -m 700 -p "$STORE/workspaces/$E2E_SESSION"
python3 - "$STORE/workspaces/$E2E_SESSION/$WS_ID.json" "$WS_ID" <<'PY'
import json,os,sys
path,ws=sys.argv[1:]
with open(path,"w") as f:
    json.dump({"version":1,"worktree_path":f"workspace:{ws}","state":"bound","issue_identifier":"project-e2e"},f)
os.chmod(path,0o600)
PY
"$BOARD_BIN" import work-store --json >"$E2E_TMP/import.json"
state_of() { brpc linear.state.get "{\"space\":\"$WS_ID\"}" >"$E2E_TMP/state.json"; }
state_of
python3 - "$E2E_TMP/state.json" "$WS_ID" "$E2E_SESSION" <<'PY' || fail "the import did not bind $WS_ID in session $E2E_SESSION: $(cat "$E2E_TMP/import.json")"
import json,sys
state=json.load(open(sys.argv[1])); ws,session=sys.argv[2:]
raise SystemExit(0 if any(b["space"]==ws and b["herdr_session"]==session for b in state["space_bindings"]) else 1)
PY
ok "space $WS_ID is bound in session $E2E_SESSION"

WORKTREE="$E2E_TMP/worktree"
mkdir -p "$WORKTREE/.git" "$WORKTREE/src"
WORKTREE="$(cd "$WORKTREE" && pwd -P)"

report() {  # report <issue identifier> — one save_issue PostToolUse payload from the agent pane
  python3 - "$1" "$WORKTREE/src" >"$E2E_TMP/payload.json" <<'PY'
import json,sys
issue,cwd=sys.argv[1:]
saved={"id":issue,"uuid":"3f1c1b0e-9f3a-4c61-8d0e-2a4b5c6d7e8f","title":"An e2e ticket",
       "createdAt":"2026-09-30T10:00:00.000Z","updatedAt":"2026-09-30T11:00:00.000Z"}
print(json.dumps({
    "session_id":"e2e-44","transcript_path":"/nonexistent/transcript.jsonl","cwd":cwd,
    "permission_mode":"default","hook_event_name":"PostToolUse",
    "tool_name":"mcp__claude_ai_Linear__save_issue","tool_input":{"title":"An e2e ticket"},
    "tool_response":[{"type":"text","text":json.dumps(saved)}],
    "tool_use_id":"toolu_e2e","duration_ms":120,
    "mcp_server":{"name":"claude_ai_Linear","source":"claude.ai"}}))
PY
  local rc=0
  ( cd "$WORKTREE/src"
    export HERDR_PANE_ID="$AGENT_PANE" HERDR_WORKSPACE_ID="$WS_ID"
    unset BOARD_CARD_ID BOARD_RUN_ID
    "$BOARD_BIN" linear report <"$E2E_TMP/payload.json" >"$E2E_TMP/report.out" 2>"$E2E_TMP/report.err" ) || rc=$?
  [ "$rc" -eq 0 ] || fail "board linear report exited $rc: $(cat "$E2E_TMP/report.err")"
  [ ! -s "$E2E_TMP/report.out" ] || fail "board linear report wrote to stdout"
}

step "save_issue from the known, unbound pane links its worktree to the saved issue"
report TEAM-123
state_of
python3 - "$E2E_TMP/state.json" "$WORKTREE" <<'PY' || fail "the worktree was not linked to TEAM-123: $(cat "$E2E_TMP/report.err")"
import json,sys
state=json.load(open(sys.argv[1]))
raise SystemExit(0 if any(b["worktree_path"]==sys.argv[2] and b["issue"]=="TEAM-123" for b in state["worktree_bindings"]) else 1)
PY
brpc linear.activity.list "{\"space\":\"$WS_ID\"}" >"$E2E_TMP/activity.json"
python3 - "$E2E_TMP/activity.json" "$AGENT_PANE" "$HERDR_SOCKET_PATH" <<'PY' || fail "the activity row does not name the save and the agent pane"
import json,sys
rows=json.load(open(sys.argv[1]))["activity"]; pane,socket=sys.argv[2:]
raise SystemExit(0 if any(a["tool_name"]=="mcp__claude_ai_Linear__save_issue" and a["issue"]=="TEAM-123"
    and a["claims"]["herdr_pane_id"]==pane and a["claims"]["herdr_socket"]==socket for a in rows) else 1)
PY
ok "worktree $WORKTREE -> TEAM-123, recorded against pane $AGENT_PANE"

step "A second save_issue from the now-bound session becomes a suggestion, not a rebind"
report TEAM-124
state_of
python3 - "$E2E_TMP/state.json" "$WORKTREE" <<'PY' || fail "the second save did not leave a TEAM-124 suggestion with TEAM-123 still bound"
import json,sys
state=json.load(open(sys.argv[1])); path=sys.argv[2]
suggested=any(m["issue"]=="TEAM-124" and m["kind"]=="suggestion" for m in state["marks"])
bound=[b["issue"] for b in state["worktree_bindings"] if b["worktree_path"]==path]
raise SystemExit(0 if suggested and bound==["TEAM-123"] else 1)
PY
ok "TEAM-124 is a suggestion mark; the worktree stays on TEAM-123"

echo; echo "44-linear-report-link: PASS"
