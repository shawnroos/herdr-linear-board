#!/usr/bin/env bash
# 40-linear-mode.sh — `board linear snapshot` runs the work plugin's snapshot script for a real
# herdr space and attaches LIVE pane status read from the origin session; a plugin below the
# version floor is refused with protocol code 6 without touching herdr.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_enable_fake_pi
export FAKE_PI_SLEEP=300
e2e_init
e2e_build
e2e_isolate

# A stand-in plugin root. The daemon runs `bin/work-snapshot.sh` in a from-scratch environment,
# so the script reads the document the scenario prepares from a path baked in at write time; the
# real plugin's Linear and record reads are covered by its own suite (plugins/work in shrimpshack).
PLUGIN_ROOT="$E2E_TMP/work-plugin"
DOC_PATH="$E2E_TMP/linear-doc.json"
mkdir -p "$PLUGIN_ROOT/.claude-plugin" "$PLUGIN_ROOT/bin"
printf '{"name":"work","version":"0.3.0"}\n' >"$PLUGIN_ROOT/.claude-plugin/plugin.json"
cat >"$PLUGIN_ROOT/bin/work-snapshot.sh" <<SCRIPT
#!/usr/bin/env bash
set -u
case "\${1:-}" in ''|*[!A-Za-z0-9_:-]*) exit 2 ;; esac
printf '%s\n' "\$1" >>"$E2E_TMP/snapshot-args"
env >"$E2E_TMP/snapshot-env"
cat "$DOC_PATH"
SCRIPT
chmod +x "$PLUGIN_ROOT/bin/work-snapshot.sh"
export BOARD_WORK_PLUGIN_ROOT="$PLUGIN_ROOT"
e2e_daemon_start

e2e_ws_standard linear-mode

step "HERDR MUTATION: dispatch a held managed-Pi card so one pane carries a live agent status"
# A plain shell pane reports no agent status (herdr answers `unknown` for it); the checked-in
# fake `pi` reports its lifecycle the way the real integration does, so its pane has one.
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"
card_json="$("$BOARD_BIN" card new --title linear-mode --description 'hold a pane for the snapshot' \
  --harness pi --model p17/pi-model --effort low --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)"
e2e_board_herdr_mutate -- move "$CARD_ID" "$EXEC_ID" --json >/dev/null
PANE_ID=""
for _ in $(seq 1 100); do
  PANE_ID="$(card_field "$CARD_ID" 'runs[-1].herdr_pane_id' 2>/dev/null || true)"
  [ -n "$PANE_ID" ] && break
  sleep 0.1
done
[ -n "$PANE_ID" ] || fail "the run never recorded a pane"
live=""
for _ in $(seq 1 100); do
  live="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json,sys
for p in json.load(sys.stdin).get("panes", []):
    if p.get("pane_id") == sys.argv[1]:
        print(p.get("agent_status") or ""); sys.exit(0)
print("")' "$PANE_ID")"
  case "$live" in idle|working|blocked) break ;; esac
  sleep 0.1
done
case "$live" in idle|working|blocked) ;; *) fail "herdr never reported a live status for $PANE_ID (got '$live')" ;; esac
TAB_ID="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json,sys
for p in json.load(sys.stdin).get("panes", []):
    if p.get("pane_id") == sys.argv[1]:
        print(p["tab_id"]); sys.exit(0)
sys.exit(1)' "$PANE_ID")" || fail "pane $PANE_ID is not listed in workspace $WS_ID"
ok "managed pane $PANE_ID in tab $TAB_ID reports $live"

python3 - "$DOC_PATH" "$WS_ID" "$TAB_ID" "$PANE_ID" <<'PY'
import json,sys
path,ws,tab,pane=sys.argv[1:]
doc={"schema":1,
 "workspace":{"id":ws,"label":"linear-mode","live":True},
 "mapping":{"status":"ok","source":"default","space":"project","tab":"work","pane":"session"},
 "record":{"status":"ok","state":"bound","project_id":"p-e2e"},
 "project":{"id":"p-e2e","name":"E2E Project","team_key":"E2E","url":None},
 "view":{"status":"none","id":None,"name":None,"layout":None},
 "linear":{"status":"ok","cache_age_seconds":None,"truncated":False},
 "herdr":{"status":"ok","version":"0.9.0"},
 "groups":[{"key":"st-todo","label":"Todo","issues":["E2E-1"]}],
 "issues":{"E2E-1":{"id":"i-1","identifier":"E2E-1","title":"Linear mode e2e","url":None,
   "state":{"id":"st-todo","name":"Todo","type":"unstarted"},"assignee":None,"priority":2,"labels":[],"stale":False,
   "bindings":[{"worktree_path":"/nonexistent/e2e","state":"worktree_missing","tab":{"id":tab,"label":"linear-mode"},"panes":[pane]}]}},
 "unmapped":[]}
json.dump(doc,open(path,"w"))
PY

step "board linear snapshot reaches the origin herdr for live pane status"
OUT="$("$BOARD_BIN" linear snapshot "$WS_ID" --json)"
printf '%s' "$OUT" | python3 -c '
import json,sys
doc=json.load(sys.stdin); pane=sys.argv[1]
assert doc["project"]["name"]=="E2E Project", doc["project"]
assert "E2E-1" in doc["issues"], list(doc["issues"])
status=doc["pane_status"].get(pane)
assert status in ("idle","working","blocked"), ("live pane status expected, got", status, doc["pane_status"])
' "$PANE_ID" || fail "snapshot did not carry a live status for pane $PANE_ID: $OUT"
grep -qx "$WS_ID" "$E2E_TMP/snapshot-args" || fail "the plugin script was not run for $WS_ID"
python3 - "$E2E_TMP/snapshot-env" "$HERDR_SOCKET_PATH" <<'PY' || fail "the script did not receive the origin session socket"
import os,sys
env=dict(l.rstrip("\n").split("=",1) for l in open(sys.argv[1]) if "=" in l)
got=env.get("HERDR_SOCKET_PATH","")
# The daemon normalises the socket path; compare the resolved files, not the spelling.
raise SystemExit(0 if got and os.path.realpath(got)==os.path.realpath(sys.argv[2]) else 1)
PY
! grep -q '^BOARD_DB=' "$E2E_TMP/snapshot-env" || fail "the daemon leaked its own environment into the script"
ok "live pane status attached from the origin session; child env is the allowlist only"

step "A refused argument stops before herdr: exit 2 from the script surfaces as protocol code 6"
set +e
ERR="$("$BOARD_BIN" linear snapshot 'bad id' --json 2>&1 >/dev/null)"; rc=$?
set -e
[ "$rc" -eq 6 ] || fail "expected exit 6 for a refused workspace id, got $rc: $ERR"
printf '%s' "$ERR" | python3 -c 'import json,sys; e=json.load(sys.stdin)["error"]; assert e["code"]==6, e' \
  || fail "error envelope did not carry code 6: $ERR"
ok "refused argument -> code 6"

step "A plugin below the version floor is refused with code 6 and both versions named"
printf '{"name":"work","version":"0.1.0"}\n' >"$PLUGIN_ROOT/.claude-plugin/plugin.json"
set +e
ERR="$("$BOARD_BIN" linear snapshot "$WS_ID" --json 2>&1 >/dev/null)"; rc=$?
set -e
[ "$rc" -eq 6 ] || fail "expected exit 6 for an old plugin, got $rc: $ERR"
printf '%s' "$ERR" | grep -q '0.1.0' && printf '%s' "$ERR" | grep -q '0.3.0' \
  || fail "the version error did not name both versions: $ERR"
ok "version floor enforced"

echo; echo "40-linear-mode: PASS"
