#!/usr/bin/env bash
# 41-linear-bind-handoff.sh — `linear.bind_handoff` opens exactly one unfocused `bind` tab in the
# caller's space and starts a REAL interactive Claude there whose startup argv is the plugin's bind
# line; a refused id or working directory creates no tab. When the installed work plugin is at least
# the version floor the skill's confirmation question is declined and the plugin store must be unchanged.
#
# Not provider-free: this scenario starts the person's own `claude` with their real HOME, because
# the bind skill, its trust dialog and its credentials live there. It skips (exit 3) on a machine
# without `claude` or without a trusted directory under the projects root.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

BIND_PROJECT=e2e-project
BIND_FLOOR="$E2E_PLUGIN_VERSION_FLOOR"

# lib.sh points HOME at the scenario root, and run-all starts children without HOME at all, so the
# person's real home comes from the password database.
REAL_HOME="$(python3 -c 'import os,pwd; print(pwd.getpwuid(os.getuid()).pw_dir)')"
CLAUDE_BIN=""
for candidate in "$REAL_HOME/.local/bin/claude" "$(type -P claude 2>/dev/null || true)"; do
  [[ "$candidate" == /* ]] && [ -x "$candidate" ] && { CLAUDE_BIN="$candidate"; break; }
done
[ -n "$CLAUDE_BIN" ] || skip "no claude executable in $REAL_HOME/.local/bin or on PATH"

PROJECTS_ROOT="$REAL_HOME/projects"
# A directory Claude already trusts, so no trust dialog stands between startup and the skill.
BIND_CWD="$(python3 - "$REAL_HOME/.claude.json" "$PROJECTS_ROOT" <<'PY' || true
import json,os,sys
try: projects=json.load(open(sys.argv[1])).get("projects",{})
except Exception: sys.exit(1)
root=os.path.realpath(sys.argv[2])
trusted=sorted(p for p,v in projects.items()
               if isinstance(v,dict) and v.get("hasTrustDialogAccepted") is True
               and os.path.isdir(p) and os.path.realpath(p).startswith(root+os.sep))
if not trusted: sys.exit(1)
print(trusted[0])
PY
)"
[ -n "$BIND_CWD" ] || skip "no Claude-trusted directory under $PROJECTS_ROOT (see $REAL_HOME/.claude.json)"

# Mirrors ops/linear.rs: the `user`-scope record for work@shrimpshack, then its plugin.json version.
PLUGIN_VERSION="$(python3 - "$REAL_HOME/.claude/plugins/installed_plugins.json" <<'PY' || true
import json,os,sys
try:
    records=json.load(open(sys.argv[1]))["plugins"]["work@shrimpshack"]
    root=next(r["installPath"] for r in records if r.get("scope")=="user" and r.get("installPath"))
    print(json.load(open(os.path.join(root,".claude-plugin","plugin.json")))["version"])
except Exception:
    sys.exit(1)
PY
)"
[ -n "$PLUGIN_VERSION" ] || PLUGIN_VERSION=none
plugin_at_floor() {
  python3 - "$PLUGIN_VERSION" "$BIND_FLOOR" <<'PY'
import sys
def triple(v):
    out=[]
    for part in (v.split("-",1)[0].split(".")+["0","0","0"])[:3]:
        out.append(int(part) if part.isdigit() else 0)
    return tuple(out)
sys.exit(0 if triple(sys.argv[1])>=triple(sys.argv[2]) else 1)
PY
}

# The pane shells of the ephemeral session read this generated startup file (through the server's
# ZDOTDIR, as the fake-managed scenarios do) and never the person's own rc files. HOME is the real
# one so Claude finds its credentials, trust and plugins; `claude` resolves to a recorder that notes
# its argv, cwd and pid and then execs the real binary.
trap e2e_cleanup EXIT
e2e_scenario_root_ensure
BIND_EVIDENCE="$E2E_SCENARIO_ROOT/bind-evidence"
BIND_ZDOT="$E2E_SCENARIO_ROOT/zdot"
mkdir -m 700 -p "$BIND_EVIDENCE/bin" "$BIND_ZDOT"
cat >"$BIND_EVIDENCE/bin/claude" <<SCRIPT
#!/bin/bash
python3 -c 'import json,os,sys; json.dump({"argv":sys.argv[1:],"cwd":os.getcwd()},open("$BIND_EVIDENCE/argv.json","w"))' "\$@"
printf '%s\n' "\$\$" >"$BIND_EVIDENCE/claude.pid"
exec "$CLAUDE_BIN" "\$@"
SCRIPT
chmod 700 "$BIND_EVIDENCE/bin/claude"
{
  printf 'export HOME=%q\n' "$REAL_HOME"
  printf 'export PATH=%q\n' "$BIND_EVIDENCE/bin:$(dirname "$CLAUDE_BIN"):/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
  printf 'export BASH_ENV=/dev/null ENV=/dev/null\n'
} >"$BIND_ZDOT/.zshenv"
command cp -f "$BIND_ZDOT/.zshenv" "$BIND_ZDOT/.zshrc"
command cp -f "$BIND_ZDOT/.zshenv" "$E2E_SCENARIO_ROOT/.bashrc"
command cp -f "$BIND_ZDOT/.zshenv" "$E2E_SCENARIO_ROOT/.bash_profile"
command cp -f "$BIND_ZDOT/.zshenv" "$E2E_SCENARIO_ROOT/.profile"
export E2E_FAKE_MANAGED_ZDOT=1 E2E_MANAGED_ZDOTDIR="$BIND_ZDOT"
export PATH="$BIND_EVIDENCE/bin:$PATH"

e2e_init
e2e_build
e2e_isolate
export HERDR_LINEAR_PROJECTS_ROOT="$PROJECTS_ROOT"
e2e_daemon_start

e2e_ws_standard bind-e2e

# Identity-gated like e2e_board_herdr_mutate; there is no CLI verb for the handoff. Prints the raw
# response line and returns board-rpc's exit code.
bind_handoff_rpc() {
  e2e_process_identity_verify "${E2E_DAEMON_PID:-}" "${E2E_DAEMON_IDENTITY:-}" \
    || fail "refusing linear.bind_handoff: daemon identity does not match"
  e2e_session_identity_verify "$E2E_SESSION_PID" "$E2E_SESSION_IDENTITY" \
    || fail "refusing linear.bind_handoff: session identity does not match"
  mut "board rpc linear.bind_handoff $1 (identity gated)" >&2
  python3 "$BOARD_RPC_BIN" linear.bind_handoff "$1"
}

handoff_params() {
  python3 -c 'import json,sys; print(json.dumps({"space":sys.argv[1],"project":sys.argv[2],"working_directory":sys.argv[3],"origin_socket":sys.argv[4]}))' \
    "$WS_ID" "$1" "$2" "$HERDR_SOCKET_PATH"
}

tabs_json() { hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}"; }
tab_count() { tabs_json | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["tabs"]))'; }

store_digest() {
  python3 - "$REAL_HOME/.claude/work" <<'PY'
import hashlib,os,sys
root=sys.argv[1]; h=hashlib.sha256()
for sub in ("bindings","workspaces"):
    base=os.path.join(root,sub)
    for dirpath,dirs,files in os.walk(base):
        dirs.sort()
        for name in sorted(files):
            path=os.path.join(dirpath,name)
            h.update(os.path.relpath(path,root).encode()+b"\0")
            try: h.update(open(path,"rb").read())
            except OSError: h.update(b"<unreadable>")
print(h.hexdigest())
PY
}

# Closing a new tab's only pane closes the tab and hangs up Claude. Runs even in keep mode: a
# waiting Claude must never outlive the scenario.
bind_pane_close_owned() {
  local pane="$1" pid="" i
  e2e_session_target_verify "$E2E_SESSION_PID" "$E2E_SESSION_IDENTITY" "$E2E_SESSION_SOCKET" || {
    printf 'E2E FAIL: refusing bind pane close %s: session identity does not match\n' "$pane" >&2; return 1;
  }
  if hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json,sys
sys.exit(0 if any(p["pane_id"]==sys.argv[1] for p in json.load(sys.stdin)["panes"]) else 1)' "$pane"; then
    e2e_hrpc_mutate -- pane.close "{\"pane_id\":\"$pane\"}" >/dev/null || return 1
  fi
  [ -f "$BIND_EVIDENCE/claude.pid" ] && pid="$(cat "$BIND_EVIDENCE/claude.pid")"
  [ -n "$pid" ] || return 0
  for (( i=0; i<50; i++ )); do
    kill -0 "$pid" 2>/dev/null || { echo "  claude pid $pid exited"; return 0; }
    sleep 0.2
  done
  printf 'E2E FAIL: claude pid %s still running 10s after its pane closed\n' "$pid" >&2
  return 1
}

step "Refused arguments stop before herdr and create no tab"
before="$(tab_count)"
for params in "$(handoff_params -rf "$BIND_CWD")" "$(handoff_params "$BIND_PROJECT" /tmp)"; do
  set +e
  resp="$(bind_handoff_rpc "$params" 2>/dev/null)"; rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "a refused handoff returned success: $resp"
  printf '%s' "$resp" | python3 -c 'import json,sys; e=json.load(sys.stdin)["error"]; assert e["message"], e' \
    || fail "a refused handoff carried no error envelope: $resp"
  [ "$(tab_count)" = "$before" ] || fail "a refused handoff created a tab: $resp"
done
ok "project '-rf' and a working directory outside the roots are refused; tab count stays $before"

step "HERDR MUTATION: hand off a bind for $WS_ID / $BIND_PROJECT in $BIND_CWD"
FOCUSED_BEFORE="$(tabs_json | python3 -c 'import json,sys; print(next((t["tab_id"] for t in json.load(sys.stdin)["tabs"] if t["focused"]), ""))')"
STORE_BEFORE="$(store_digest)"
resp="$(bind_handoff_rpc "$(handoff_params "$BIND_PROJECT" "$BIND_CWD")")" \
  || fail "linear.bind_handoff failed: $resp (see $E2E_TMP/daemon.log)"
BIND_TAB="$(printf '%s' "$resp" | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["tab_id"])')"
BIND_PANE="$(printf '%s' "$resp" | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["pane_id"])')"
printf -v _close_cmd 'bind_pane_close_owned %q' "$BIND_PANE"
e2e_defer "$_close_cmd"
echo "  bind tab $BIND_TAB, pane $BIND_PANE"

tabs_json | python3 -c '
import json,sys
tabs=json.load(sys.stdin)["tabs"]; before=int(sys.argv[1]); tab=sys.argv[2]; focused=sys.argv[3]
assert len(tabs)==before+1, ("expected exactly one new tab", len(tabs), before)
binds=[t for t in tabs if t["label"]=="bind"]
assert len(binds)==1 and binds[0]["tab_id"]==tab, ("expected one bind tab with the returned id", binds)
assert binds[0]["focused"] is False, ("the bind tab took focus", binds[0])
assert focused=="" or any(t["tab_id"]==focused and t["focused"] for t in tabs), ("focus moved off", focused)
' "$before" "$BIND_TAB" "$FOCUSED_BEFORE" || fail "tab shape after the handoff is wrong: $(tabs_json)"
ok "exactly one new unfocused tab labelled bind; the focused tab is unchanged"

for (( i=0; i<300; i++ )); do [ -s "$BIND_EVIDENCE/argv.json" ] && break; sleep 0.2; done
[ -s "$BIND_EVIDENCE/argv.json" ] || fail "claude never started in $BIND_PANE"
python3 - "$BIND_EVIDENCE/argv.json" "/work:bind --space $WS_ID --project $BIND_PROJECT" "$BIND_CWD" <<'PY' \
  || fail "claude argv/cwd is wrong: $(cat "$BIND_EVIDENCE/argv.json")"
import json,os,sys
doc=json.load(open(sys.argv[1]))
assert doc["argv"]==[sys.argv[2]]
assert os.path.realpath(doc["cwd"])==os.path.realpath(sys.argv[3])
PY
ok "claude started with argv exactly ['/work:bind --space $WS_ID --project $BIND_PROJECT'] in $BIND_CWD"

agent=""
for (( i=0; i<150; i++ )); do
  agent="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json,sys
for p in json.load(sys.stdin)["panes"]:
    if p["pane_id"]==sys.argv[1]: print(p.get("agent") or ""); break' "$BIND_PANE")"
  [ "$agent" = claude ] && break
  sleep 0.2
done
[ "$agent" = claude ] || fail "herdr does not report claude on $BIND_PANE (got '$agent')"
ok "herdr reports agent claude on the bind pane"

if plugin_at_floor; then
  step "HERDR MUTATION: the skill asks before it writes; decline it"
  screen=""
  for (( i=0; i<120; i++ )); do
    screen="$(hrpc pane.read "{\"pane_id\":\"$BIND_PANE\",\"source\":\"recent\"}" | python3 -c '
import json,sys
doc=json.load(sys.stdin)
def text(o):
    if isinstance(o,str): return o
    if isinstance(o,dict): return "\n".join(text(v) for v in o.values())
    if isinstance(o,list): return "\n".join(text(v) for v in o)
    return ""
print(text(doc))')"
    # Claude echoes the bind line more than once, so lines carrying it are not evidence: the
    # question must name both ids on its own lines and ask something.
    if printf '%s' "$screen" | python3 -c '
import sys
t="\n".join(l for l in sys.stdin.read().splitlines() if "/work:bind" not in l)
sys.exit(0 if sys.argv[1] in t and sys.argv[2] in t and "?" in t else 1)' "$BIND_PROJECT" "$WS_ID"; then
      break
    fi
    screen=""
    sleep 1
  done
  [ -n "$screen" ] || fail "no confirmation naming $WS_ID and $BIND_PROJECT appeared within 120s"
  ok "the skill's confirmation names the space and project"
  e2e_hrpc_mutate -- pane.send_keys "{\"pane_id\":\"$BIND_PANE\",\"keys\":[\"esc\"]}" >/dev/null
  sleep 5
  [ "$(store_digest)" = "$STORE_BEFORE" ] || fail "declining the bind changed the plugin store"
  ok "declined; the plugin store under $REAL_HOME/.claude/work is unchanged"
else
  echo "  skipped: installed work plugin $PLUGIN_VERSION < $BIND_FLOOR; reinstall the plugin to run this check (confirmation question and decline)"
fi

step "HERDR MUTATION: close the bind pane so no waiting Claude outlives the scenario"
bind_pane_close_owned "$BIND_PANE"
[ "$(tab_count)" = "$before" ] || fail "the bind tab is still open after its pane closed: $(tabs_json)"
ok "bind tab closed; tab count back to $before"
if [ "$(store_digest)" != "$STORE_BEFORE" ]; then
  echo "  note: the plugin store changed while the installed plugin ($PLUGIN_VERSION) ran the bind line"
fi

echo; echo "41-linear-bind-handoff: PASS"
