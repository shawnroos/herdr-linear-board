#!/usr/bin/env bash
# 42-linear-issue.sh — `board linear issue` runs the work plugin's per-issue read and returns the
# document the issue page draws from; a plugin that ships no issue script is refused with protocol
# code 7, which is a DIFFERENT remedy from the code 6 everything else on the plugin path returns,
# and the rest of Linear mode keeps working while it is missing.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_init
e2e_build
e2e_isolate

# The document is the vendored fixture: the work plugin's own output against its fake Linear
# (crates/board-core/tests/fixtures/linear-issue/, pinned by sha256 in VERSION). Using it here
# means this scenario and the plugin's own suite are reading one contract rather than two.
FIXTURES="$REPO_ROOT/crates/board-core/tests/fixtures/linear-issue"
PLUGIN_ROOT="$E2E_TMP/work-plugin"
mkdir -p "$PLUGIN_ROOT/.claude-plugin" "$PLUGIN_ROOT/bin"
# The floor comes from the source of truth, not a literal: a bump would
# otherwise leave this scenario asserting against a version the board refuses.
printf '{"name":"work","version":"%s"}\n' "$E2E_PLUGIN_VERSION_FLOOR" \
    >"$PLUGIN_ROOT/.claude-plugin/plugin.json"

# The snapshot script, so this scenario can prove ONE missing script does not take the rest of
# Linear mode down with it.
cat >"$PLUGIN_ROOT/bin/work-snapshot.sh" <<SCRIPT
#!/usr/bin/env bash
set -u
case "\${1:-}" in ''|*[!A-Za-z0-9_:-]*) exit 2 ;; esac
cat "$REPO_ROOT/crates/board-core/tests/fixtures/linear-snapshot/unbound.json"
SCRIPT
chmod +x "$PLUGIN_ROOT/bin/work-snapshot.sh"

write_issue_script() {  # write_issue_script <fixture file>
    cat >"$PLUGIN_ROOT/bin/work-issue.sh" <<SCRIPT
#!/usr/bin/env bash
set -u
case "\${1:-}" in ''|*[!A-Za-z0-9_-]*) exit 2 ;; esac
printf '%s\n' "\$1" >>"$E2E_TMP/issue-args"
cat "$FIXTURES/$1"
SCRIPT
    chmod +x "$PLUGIN_ROOT/bin/work-issue.sh"
}

export BOARD_WORK_PLUGIN_ROOT="$PLUGIN_ROOT"
e2e_daemon_start

step "A full read returns every section the issue page draws"
write_issue_script full.json
DOC="$("$BOARD_BIN" linear issue WEB-3318 --json)"
printf '%s' "$DOC" >"$E2E_TMP/full-doc.json"
# The document goes through a FILE, not a heredoc: `python3 - <<PY` makes the
# heredoc stdin, so a piped document never reaches json.load and the check
# fails for a reason that has nothing to do with the document.
python3 - "$E2E_TMP/full-doc.json" <<'PY' || fail "the document did not carry every section: $DOC"
import json, sys
d = json.load(open(sys.argv[1]))
assert d["schema"] == 1, d
assert d["status"] == "ok", d
assert d["truncated"] == [], d
i = d["issue"]
for key in ("description", "children", "relations", "comments", "history",
            "milestone", "cycle", "estimate", "due_date", "parent"):
    assert key in i, f"missing {key}"
assert i["identifier"] == "WEB-3318", i
assert len(i["children"]) >= 1, i
# R9: every linked row can open its own page from the row alone.
linked = [i["parent"]] + i["children"] + [r["issue"] for r in i["relations"]]
for row in linked:
    assert row["identifier"] and row["title"] and (row.get("state") or {}).get("name"), row
# Both ends of a relation, which is what separates blocks from blocked by.
directions = {r["direction"] for r in i["relations"]}
assert directions == {"inward", "outward"}, directions
PY
[ "$(cat "$E2E_TMP/issue-args")" = "WEB-3318" ] || fail "the script was not given the issue id"
ok "full read"

step "A read that stopped at its page cap is partial and names what it cut"
write_issue_script truncated.json
DOC="$("$BOARD_BIN" linear issue WEB-3318 --json)"
printf '%s' "$DOC" | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["status"] == "partial", d
assert d["truncated"], d
assert d["issue"] is not None, "a partial read still carries the issue"
' || fail "a truncated read was not reported as partial: $DOC"
ok "partial read names what was cut"

step "Linear being unreachable is a DOCUMENT, not an error code"
write_issue_script unavailable.json
DOC="$("$BOARD_BIN" linear issue WEB-3318 --json)"
printf '%s' "$DOC" | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["status"] == "unavailable", d
assert d["issue"] is None, d
assert d["message"], d
' || fail "an unreachable Linear did not arrive as a document: $DOC"
ok "unavailable read is a document the page can keep its fields under"

step "A refused issue id stops before the script runs"
: >"$E2E_TMP/issue-args"
set +e
ERR="$("$BOARD_BIN" linear issue 'bad id' --json 2>&1 >/dev/null)"; rc=$?
set -e
[ "$rc" -eq 1 ] || fail "expected exit 1 for a refused issue id, got $rc: $ERR"
[ ! -s "$E2E_TMP/issue-args" ] || fail "a refused id still ran the script"
ok "refused id -> code 1, nothing ran"

step "AE3: a plugin with no issue script is code 7, and the rest of Linear mode keeps working"
rm -f "$PLUGIN_ROOT/bin/work-issue.sh"
set +e
ERR="$("$BOARD_BIN" linear issue WEB-3318 --json 2>&1 >/dev/null)"; rc=$?
set -e
[ "$rc" -eq 7 ] || fail "expected exit 7 for a plugin with no issue script, got $rc: $ERR"
printf '%s' "$ERR" | python3 -c 'import json,sys; e=json.load(sys.stdin)["error"]; assert e["code"]==7, e' \
  || fail "error envelope did not carry code 7: $ERR"
printf '%s' "$ERR" | grep -q 'work-issue.sh' || fail "the error did not name the missing script: $ERR"
# The distinction that matters: 7 is "update the plugin", 6 is worth retrying.
"$BOARD_BIN" linear snapshot wA --json >/dev/null \
  || fail "one missing script took the rest of Linear mode down with it"
ok "missing script -> code 7; the snapshot still works"

step "A plugin below the version floor is still code 6, not code 7"
write_issue_script full.json
# Deliberately a literal, and deliberately far below any floor this board will
# ever carry: the point of the step is a plugin the version check refuses.
printf '{"name":"work","version":"0.1.0"}\n' >"$PLUGIN_ROOT/.claude-plugin/plugin.json"
set +e
ERR="$("$BOARD_BIN" linear issue WEB-3318 --json 2>&1 >/dev/null)"; rc=$?
set -e
[ "$rc" -eq 6 ] || fail "expected exit 6 for an old plugin, got $rc: $ERR"
ok "version floor is code 6; a missing script is code 7"

echo; echo "42-linear-issue: PASS"
