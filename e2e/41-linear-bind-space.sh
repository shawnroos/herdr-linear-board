#!/usr/bin/env bash
# 41-linear-bind-space.sh — bind a herdr space to a Linear project from the board itself: the real
# TUI in Linear mode, in a pane of an unbound disposable workspace, shows the not-bound screen; `s`
# and Enter open the project picker filled from the scenario's own fake Linear
# (e2e/fake-linear.py), and Enter on a project sends `linear.space.bind`. The space is then bound in
# boardd's local state, the board redraws with the project's issues, and `board linear snapshot`
# reads it as bound. No Claude and no work-plugin script is involved.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_init
e2e_build
e2e_isolate

FIXTURE="$E2E_TMP/linear-fixture.json"
python3 - "$FIXTURE" <<'PY'
import json,sys
fixture = [
    ["projects(", {"data": {"projects": {
        "nodes": [{"id": "p-e2e", "name": "E2E Project",
                   "teams": {"nodes": [{"id": "team-e2e", "key": "E2E", "name": "E2E"}]}}],
        "pageInfo": {"hasNextPage": False, "endCursor": None}}}}],
    ["issues(", {"data": {"issues": {
        "nodes": [{"id": "id-E2E-41", "identifier": "E2E-41", "title": "Bound from the board",
                   "url": None, "state": {"id": "s-todo", "name": "Todo", "type": "unstarted"},
                   "team": {"id": "team-e2e", "key": "E2E", "name": "E2E"}, "assignee": None,
                   "priority": 2, "labels": {"nodes": []}}],
        "pageInfo": {"hasNextPage": False, "endCursor": None}}}}],
    ["project(id", {"data": {"project": {
        "id": "p-e2e", "name": "E2E Project", "url": None,
        "teams": {"nodes": [{"id": "team-e2e", "key": "E2E", "name": "E2E", "states": {"nodes": [
            {"id": "s-todo", "name": "Todo", "type": "unstarted"},
            {"id": "s-done", "name": "Done", "type": "completed"}]}}]}}}}],
]
json.dump(fixture, open(sys.argv[1], "w"))
PY
e2e_fake_linear_start "$FIXTURE"
e2e_daemon_start

e2e_ws_standard bind-space

# wait_screen <pane> <substring> — poll the rendered pane until <substring> shows.
wait_screen() {
  local pane="$1" needle="$2" screen="" i
  for (( i=0; i<150; i++ )); do
    screen="$("$HERDR_BIN" pane read "$pane" --source recent-unwrapped --lines 200 2>/dev/null || true)"
    printf '%s\n' "$screen" | grep -Fq "$needle" && return 0
    sleep 0.1
  done
  printf '%s\n' "$screen" >"$E2E_TMP/last-screen.txt"
  fail "pane did not render '$needle' (last screen in $E2E_TMP/last-screen.txt)"
}

space_binding() {  # the project id this session binds the space to, or empty
  brpc linear.state.get "{\"space\":\"$WS_ID\",\"herdr_socket\":\"$HERDR_SOCKET_PATH\"}" | python3 -c '
import json,sys
rows=json.load(sys.stdin).get("space_bindings", [])
print(rows[0]["project_id"] if rows else "")'
}

[ -z "$(space_binding)" ] || fail "the disposable space started bound"

step "HERDR MUTATION: launch the real TUI in Linear mode in a pane of the unbound space"
TAB_JSON="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label bind-space --no-focus)"
PANE_ID="$(printf '%s' "$TAB_JSON" | jget pane_id)"
[ -n "$PANE_ID" ] || fail "could not find the pane for the bind-space tab"
e2e_herdr_mutate -- pane run "$PANE_ID" \
  "stty cols 65; HERDR_WORKSPACE_ID=$WS_ID HERDR_SOCKET_PATH=$HERDR_SOCKET_PATH BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG $BOARD_BIN tui"
wait_screen "$PANE_ID" "Press s"
ok "the not-bound screen offers the space picker"

step "HERDR MUTATION: s then Enter opens the project picker from fake Linear"
e2e_herdr_mutate -- pane send-text "$PANE_ID" s >/dev/null
e2e_herdr_mutate -- pane send-keys "$PANE_ID" enter >/dev/null
wait_screen "$PANE_ID" "E2E Project"
[ "$(linear_requests 'projects(')" -ge 1 ] || fail "the picker did not read projects from Linear"
ok "project picker lists E2E Project"

step "HERDR MUTATION: Enter on the project binds the space"
e2e_herdr_mutate -- pane send-keys "$PANE_ID" enter >/dev/null
BOUND=""
for _ in $(seq 1 100); do
  BOUND="$(space_binding)"
  [ -n "$BOUND" ] && break
  sleep 0.1
done
[ "$BOUND" = "p-e2e" ] || fail "the space was not bound to p-e2e (got '$BOUND')"
wait_screen "$PANE_ID" "E2E-41"
ok "space bound to p-e2e; the board shows its issue"

step "board linear snapshot reads the space as bound"
OUT="$("$BOARD_BIN" linear snapshot "$WS_ID" --json)"
printf '%s' "$OUT" >"$E2E_TMP/bound.json"
python3 - "$E2E_TMP/bound.json" <<'PY' || fail "the snapshot did not read the space as bound: $OUT"
import json,sys
doc=json.load(open(sys.argv[1]))
raise SystemExit(0 if doc["record"]["state"] == "bound" and doc["record"]["project_id"] == "p-e2e" else 1)
PY
ok "bound in the snapshot read"

echo; echo "41-linear-bind-space: PASS"
