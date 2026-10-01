#!/usr/bin/env bash
# 40-linear-mode.sh — a herdr space bound to a Linear project reads as a grouped board: `board
# linear snapshot` against the scenario's own fake Linear (e2e/fake-linear.py) returns the bound
# record, the project, and the project's issues grouped by its team's workflow states, with the
# space reported live from the origin herdr session. An unbound space makes no Linear call.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_init
e2e_build
e2e_isolate

FIXTURE="$E2E_TMP/linear-fixture.json"
python3 - "$FIXTURE" <<'PY'
import json,sys
def issue(identifier, state):
    return {"id": "id-" + identifier, "identifier": identifier, "title": "title " + identifier,
            "url": None, "state": {"id": state[0], "name": state[1], "type": state[2]},
            "team": {"id": "team-e2e", "key": "E2E", "name": "E2E"}, "assignee": None,
            "priority": 2, "labels": {"nodes": []}}
fixture = [
    ["issues(", {"data": {"issues": {
        "nodes": [issue("E2E-1", ("s-todo", "Todo", "unstarted")),
                  issue("E2E-2", ("s-doing", "In Progress", "started"))],
        "pageInfo": {"hasNextPage": False, "endCursor": None}}}}],
    ["project(id", {"data": {"project": {
        "id": "p-e2e", "name": "E2E Project", "url": None,
        "teams": {"nodes": [{"id": "team-e2e", "key": "E2E", "name": "E2E", "states": {"nodes": [
            {"id": "s-todo", "name": "Todo", "type": "unstarted"},
            {"id": "s-doing", "name": "In Progress", "type": "started"},
            {"id": "s-done", "name": "Done", "type": "completed"}]}}]}}}}],
]
json.dump(fixture, open(sys.argv[1], "w"))
PY
e2e_fake_linear_start "$FIXTURE"
e2e_daemon_start

e2e_ws_standard linear-mode

step "An unbound space reads as unbound without a Linear call"
OUT="$("$BOARD_BIN" linear snapshot "$WS_ID" --json)"
printf '%s' "$OUT" >"$E2E_TMP/unbound.json"
python3 - "$E2E_TMP/unbound.json" <<'PY' || fail "the unbound read was wrong: $OUT"
import json,sys
doc=json.load(open(sys.argv[1]))
raise SystemExit(0 if doc["record"]["state"] == "unbound" and not doc["groups"] else 1)
PY
[ "$(linear_requests)" -eq 0 ] || fail "an unbound space called Linear"
ok "unbound, no Linear call"

step "Bind the space to the project in this herdr session"
brpc linear.space.bind \
  "{\"space\":\"$WS_ID\",\"project\":\"p-e2e\",\"claims\":{\"herdr_socket\":\"$HERDR_SOCKET_PATH\"}}" \
  >"$E2E_TMP/bind.json"
python3 - "$E2E_TMP/bind.json" <<'PY' || fail "linear.space.bind did not return the new binding"
import json,sys
change=json.load(open(sys.argv[1]))
raise SystemExit(0 if change["before"] is None and change["after"]["project_id"] == "p-e2e" else 1)
PY
ok "space bound"

step "board linear snapshot shows the bound space's board, grouped by workflow state"
OUT="$("$BOARD_BIN" linear snapshot "$WS_ID" --json)"
printf '%s' "$OUT" >"$E2E_TMP/bound.json"
python3 - "$E2E_TMP/bound.json" "$WS_ID" <<'PY' || fail "the bound snapshot was wrong: $OUT"
import json,sys
doc=json.load(open(sys.argv[1])); ws=sys.argv[2]
groups={g["key"]: g["issues"] for g in doc["groups"]}
checks=[
    doc["record"]["state"] == "bound",
    doc["record"]["project_id"] == "p-e2e",
    doc["project"]["name"] == "E2E Project",
    doc["linear"]["status"] == "ok",
    groups.get("s-todo") == ["E2E-1"],
    groups.get("s-doing") == ["E2E-2"],
    sorted(doc["issues"]) == ["E2E-1", "E2E-2"],
    doc["workspace"]["id"] == ws and doc["workspace"]["live"] is True,
    doc["herdr"]["status"] == "ok",
]
raise SystemExit(0 if all(checks) else 1)
PY
[ "$(linear_requests 'p-e2e')" -ge 1 ] || fail "the issue read did not filter by the bound project"
ok "bound board: Todo E2E-1, In Progress E2E-2; space live"

echo; echo "40-linear-mode: PASS"
