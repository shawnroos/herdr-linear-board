#!/usr/bin/env bash
# 42-linear-issue.sh — `board linear issue` returns the whole issue the issue page draws from
# (description, sub-issues, parent, relations both ways, comments and history) in ONE GraphQL call
# to the scenario's own fake Linear (e2e/fake-linear.py). An issue Linear cannot read is a document
# with status `unavailable`, not an error code, and a refused id stops before any call.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_init
e2e_build
e2e_isolate

FIXTURE="$E2E_TMP/linear-fixture.json"
python3 - "$FIXTURE" <<'PY'
import json,sys
def state(name, kind):
    return {"id": "s-" + kind, "name": name, "type": kind}
def linked(identifier, title):
    return {"id": "id-" + identifier, "identifier": identifier, "title": title,
            "state": state("Todo", "unstarted")}
def page(nodes):
    return {"nodes": nodes, "pageInfo": {"hasNextPage": False}}
issue = {
    "id": "id-E2E-7", "identifier": "E2E-7", "title": "Read the whole issue",
    "url": None, "branchName": "e2e-7", "updatedAt": "2026-10-01T00:00:00Z", "priority": 2,
    "state": state("In Progress", "started"),
    "parent": dict(linked("E2E-1", "Parent work"), state=state("In Progress", "started")),
    "project": {"id": "p-e2e", "name": "E2E Project"},
    "projectMilestone": {"id": "m-1", "name": "Beta"}, "cycle": {"id": "c-1", "number": 4, "name": None},
    "team": {"id": "team-e2e", "key": "E2E", "name": "E2E"}, "assignee": {"id": "u-1", "name": "Ada"},
    "labels": {"nodes": [{"id": "l-1", "name": "Bug", "parent": None}]},
    "description": "Why this matters", "dueDate": "2026-10-31", "estimate": 3,
    "children": page([linked("E2E-8", "A sub-issue")]),
    "relations": page([{"id": "r-1", "type": "blocks", "relatedIssue": linked("E2E-9", "Blocked work")}]),
    "inverseRelations": page([{"id": "r-2", "type": "blocks", "issue": linked("E2E-3", "Blocking work")}]),
    "comments": page([{"id": "cm-1", "body": "Looks right", "createdAt": "2026-09-30T00:00:00Z",
                       "user": {"id": "u-1", "name": "Ada"}, "parent": None}]),
    "history": page([{"id": "h-1", "createdAt": "2026-09-29T00:00:00Z", "actor": {"id": "u-1", "name": "Ada"},
                      "fromState": {"name": "Todo"}, "toState": {"name": "In Progress"},
                      "fromAssignee": None, "toAssignee": None, "fromPriority": 2, "toPriority": 2,
                      "addedLabels": [], "removedLabels": []}]),
}
fixture = [
    ["E2E-404", {"data": {"issue": None}}],
    ["issue(id", {"data": {"issue": issue}}],
]
json.dump(fixture, open(sys.argv[1], "w"))
PY
e2e_fake_linear_start "$FIXTURE"
e2e_daemon_start

step "One issue read returns every section the issue page draws, in one GraphQL call"
DOC="$("$BOARD_BIN" linear issue E2E-7 --json)"
printf '%s' "$DOC" >"$E2E_TMP/full-doc.json"
python3 - "$E2E_TMP/full-doc.json" <<'PY' || fail "the document did not carry every section: $DOC"
import json,sys
d=json.load(open(sys.argv[1]))
i=d["issue"] or {}
linked=[i.get("parent") or {}] + i.get("children", []) + [r["issue"] for r in i.get("relations", [])]
checks=[
    d["schema"] == 1, d["status"] == "ok", d["truncated"] == [],
    i.get("identifier") == "E2E-7", i.get("description") == "Why this matters",
    len(i.get("children", [])) == 1, len(i.get("comments", [])) == 1, len(i.get("history", [])) == 1,
    (i.get("milestone") or {}).get("name") == "Beta",
    {r["direction"] for r in i.get("relations", [])} == {"inward", "outward"},
    all(row.get("identifier") and row.get("title") and (row.get("state") or {}).get("name") for row in linked),
]
raise SystemExit(0 if all(checks) else 1)
PY
[ "$(linear_requests)" -eq 1 ] || fail "the issue read made $(linear_requests) GraphQL calls, not one"
[ "$(linear_requests 'issue(id')" -eq 1 ] || fail "the one call was not the issue query"
ok "full issue in one call"

step "An issue Linear cannot read is an unavailable document, not an error"
DOC="$("$BOARD_BIN" linear issue E2E-404 --json)"
printf '%s' "$DOC" >"$E2E_TMP/missing-doc.json"
python3 - "$E2E_TMP/missing-doc.json" <<'PY' || fail "a missing issue did not arrive as a document: $DOC"
import json,sys
d=json.load(open(sys.argv[1]))
raise SystemExit(0 if d["status"] == "unavailable" and d["issue"] is None and d["message"] else 1)
PY
ok "unavailable document"

step "A refused issue id stops before any GraphQL call"
BEFORE="$(linear_requests)"
set +e
ERR="$("$BOARD_BIN" linear issue 'bad id' --json 2>&1 >/dev/null)"; rc=$?
set -e
[ "$rc" -eq 1 ] || fail "expected exit 1 for a refused issue id, got $rc: $ERR"
[ "$(linear_requests)" -eq "$BEFORE" ] || fail "a refused id still called Linear"
ok "refused id -> code 1, no call"

echo; echo "42-linear-issue: PASS"
