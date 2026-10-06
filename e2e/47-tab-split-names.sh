#!/usr/bin/env bash
# 47-tab-split-names.sh — the daemon names the tabs and splits of a Linear-bound space from their
# scope. A tab whose pane works in a worktree bound to TEAM-47 is named `TEAM-47 · <title>` from
# the scenario's own fake Linear; a split in it running an agent is `TEAM-47 · claude`. A tab
# renamed by hand keeps its name when the binding moves to TEAM-48, while the split follows the new
# binding. A tab in an unbound workspace of the same session is never renamed.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_init
e2e_build
e2e_isolate

FIXTURE="$E2E_TMP/linear-fixture.json"
python3 - "$FIXTURE" <<'PY'
import json,sys
def issue(key, title):
    return {"id": f"id-{key}", "identifier": key, "title": title, "url": None,
            "state": {"id": "s-todo", "name": "Todo", "type": "unstarted"},
            "team": {"id": "team-e2e", "key": "TEAM", "name": "Team"}, "assignee": None,
            "priority": 2, "labels": {"nodes": []}}
fixture = [
    ["issues(", {"data": {"issues": {
        "nodes": [issue("TEAM-47", "Fix login"), issue("TEAM-48", "Ship it")],
        "pageInfo": {"hasNextPage": False, "endCursor": None}}}}],
    ["project(id", {"data": {"project": {
        "id": "p-e2e", "name": "E2E Project", "url": None,
        "teams": {"nodes": [{"id": "team-e2e", "key": "TEAM", "name": "Team", "states": {"nodes": [
            {"id": "s-todo", "name": "Todo", "type": "unstarted"}]}}]}}}}],
]
json.dump(fixture, open(sys.argv[1], "w"))
PY
e2e_fake_linear_start "$FIXTURE"
e2e_daemon_start

e2e_ws_standard tab-names
BOUND_WS="$WS_ID"
e2e_ws_standard tab-names-other
OTHER_WS="$WS_ID"

tab_label() {  # tab_label <tab_id>
  hrpc tab.list '{}' | python3 -c '
import json,sys
print(next((t.get("label") or "") for t in json.load(sys.stdin)["tabs"] if t["tab_id"]==sys.argv[1]))' "$1"
}
pane_label() {  # pane_label <pane_id>
  hrpc pane.get "{\"pane_id\":\"$1\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["pane"].get("label") or "")'
}
wait_label() {  # wait_label <tab|pane> <id> <label> — poll until the label shows, or fail
  local kind="$1" id="$2" want="$3" got=""
  for _ in $(seq 1 200); do
    got="$("${kind}_label" "$id")"
    [ "$got" = "$want" ] && return 0
    sleep 0.1
  done
  fail "$kind $id is labelled '$got', not '$want'"
}
created_pane() { python3 -c 'import json,sys; r=json.load(sys.stdin); print((r.get("pane") or r.get("root_pane") or r)["pane_id"])'; }
created_tab() { python3 -c 'import json,sys; print(json.load(sys.stdin)["tab"]["tab_id"])'; }

step "Bind the space to the fake Linear project and warm its read"
brpc linear.space.bind "{\"space\":\"$BOUND_WS\",\"project\":\"p-e2e\",\"claims\":{\"herdr_socket\":\"$HERDR_SOCKET_PATH\"}}" >/dev/null
"$BOARD_BIN" linear snapshot "$BOUND_WS" --json >"$E2E_TMP/snapshot.json"
grep -q 'Fix login' "$E2E_TMP/snapshot.json" || fail "the snapshot did not read TEAM-47's title: $(cat "$E2E_TMP/snapshot.json")"
WORKTREE="$E2E_TMP/web"
mkdir -p "$WORKTREE/.git" "$WORKTREE/src"
WORKTREE="$(cd "$WORKTREE" && pwd -P)"
brpc linear.bind "{\"cwd\":\"$WORKTREE\",\"issue\":\"TEAM-47\",\"space\":\"$BOUND_WS\"}" >/dev/null
ok "space $BOUND_WS bound; worktree $WORKTREE -> TEAM-47"

step "HERDR MUTATION: a tab in the bound worktree, and an agent split in it"
TAB_JSON="$(e2e_hrpc_mutate -- tab.create "{\"workspace_id\":\"$BOUND_WS\",\"focus\":false,\"cwd\":\"$WORKTREE/src\"}")"
TAB="$(printf '%s' "$TAB_JSON" | created_tab)"
ROOT="$(printf '%s' "$TAB_JSON" | created_pane)"
SPLIT="$(e2e_hrpc_mutate -- pane.split "{\"target_pane_id\":\"$ROOT\",\"direction\":\"right\",\"focus\":false,\"cwd\":\"$WORKTREE/src\"}" | created_pane)"
e2e_hrpc_mutate -- pane.report_agent "{\"pane_id\":\"$SPLIT\",\"source\":\"e2e\",\"agent\":\"claude\",\"state\":\"idle\"}" >/dev/null
OTHER_TAB="$(hrpc tab.list "{\"workspace_id\":\"$OTHER_WS\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["tabs"][0]["tab_id"])')"
OTHER_LABEL="$(tab_label "$OTHER_TAB")"

wait_label tab "$TAB" "TEAM-47 · Fix login"
wait_label pane "$SPLIT" "TEAM-47 · claude"
wait_label pane "$ROOT" "TEAM-47"
ok "tab $TAB is 'TEAM-47 · Fix login'; split $SPLIT is 'TEAM-47 · claude'"

step "HERDR MUTATION: rename the tab by hand, then move the binding to TEAM-48"
e2e_hrpc_mutate -- tab.rename "{\"tab_id\":\"$TAB\",\"label\":\"scratch\"}" >/dev/null
brpc linear.unbind "{\"cwd\":\"$WORKTREE\",\"space\":\"$BOUND_WS\"}" >/dev/null
brpc linear.bind "{\"cwd\":\"$WORKTREE\",\"issue\":\"TEAM-48\",\"space\":\"$BOUND_WS\"}" >/dev/null
wait_label pane "$SPLIT" "TEAM-48 · claude"
[ "$(tab_label "$TAB")" = "scratch" ] || fail "the hand-set tab label was replaced by '$(tab_label "$TAB")'"
ok "the hand-set tab label survives; the split follows TEAM-48"

step "A tab in the unbound workspace keeps herdr's label"
[ "$(tab_label "$OTHER_TAB")" = "$OTHER_LABEL" ] || fail "tab $OTHER_TAB in unbound $OTHER_WS was renamed to '$(tab_label "$OTHER_TAB")'"
ok "tab $OTHER_TAB is still '$OTHER_LABEL'"

echo; echo "47-tab-split-names: PASS"
