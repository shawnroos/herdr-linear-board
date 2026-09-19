#!/usr/bin/env bash
# 13-jump-to-pane.sh — CLI focus reaches the SELECTED same-session run pane.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_AGENT_HOLD=300"
e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

step "Create a disposable target workspace and a held fake-agent pane"
e2e_ws_create jump-target; WS_ID="$E2E_WS"
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"
card_json="$("$BOARD_BIN" card new --title jump-target --description 'focus this run' \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)"
e2e_board_herdr_mutate -- move "$CARD_ID" "$EXEC_ID" --json >/dev/null
outcome="$(wait_ok "$CARD_ID")" || fail "run did not finish (outcome '$outcome')"
[ "$outcome" = "ok" ] || fail "run outcome '$outcome' (expected ok)"

step "HERDR MUTATION: board retry $CARD_ID -> a SECOND run, so the card has a run history"
# Run selection in the TUI is only observable with more than one run: the
# scenario deliberately picks a NON-newest row below.
mut "board retry $CARD_ID"
e2e_board_herdr_mutate -- retry "$CARD_ID" --json >/dev/null || fail "board retry failed"
outcome2="$(wait_runs "$CARD_ID" 2)" || fail "retry did not spawn/finish a 2nd run"
[ "$outcome2" = "ok" ] || fail "retry run outcome '$outcome2' (expected ok)"

OLD_RUN="$(card_field "$CARD_ID" 'runs[-2].id')"
OLD_PANE="$(card_field "$CARD_ID" 'runs[-2].herdr_pane_id')"
TARGET_PANE="$(card_field "$CARD_ID" 'runs[-1].herdr_pane_id')"
# `run.focus` requires an explicit run id: read it from the same card detail.
TARGET_RUN="$(card_field "$CARD_ID" 'runs[-1].id')"
[ -n "$TARGET_PANE" ] || fail "run did not record a pane"
[ -n "$TARGET_RUN" ] || fail "card detail did not expose the run id"
[ -n "$OLD_RUN" ] && [ -n "$OLD_PANE" ] || fail "first run did not keep its identity"
[ "$OLD_RUN" != "$TARGET_RUN" ] || fail "retry did not create a distinct run"
[ "$OLD_PANE" != "$TARGET_PANE" ] || fail "retry reused the first run's pane"
hrpc pane.get "{\"pane_id\":\"$TARGET_PANE\"}" >/dev/null \
  || fail "held fake-agent pane is not accessible"
ok "target pane $TARGET_PANE remains alive after board done"

# The retry reclaims the first run's ended child pane (see
# spawner/placement.rs::reclaim_prior_children), so run $OLD_RUN keeps a
# recorded pane id that no longer exists. Make that premise deterministic
# instead of assuming the reclaim order: closing an already-closed disposable
# board-owned pane is a no-op.
if hrpc pane.get "{\"pane_id\":\"$OLD_PANE\"}" >/dev/null 2>&1; then
  mut "pane.close $OLD_PANE (disposable board-owned pane of the retried run)"
  e2e_hrpc_mutate -- pane.close "{\"pane_id\":\"$OLD_PANE\"}" >/dev/null 2>&1 || true
fi
for _ in $(seq 1 30); do
  hrpc pane.get "{\"pane_id\":\"$OLD_PANE\"}" >/dev/null 2>&1 || break
  sleep .1
done
! hrpc pane.get "{\"pane_id\":\"$OLD_PANE\"}" >/dev/null 2>&1 \
  || fail "older run's pane $OLD_PANE is still alive; the dead-pane case cannot be exercised"
ok "run $OLD_RUN still records the now-dead pane $OLD_PANE"

# The kanban overlay half of this scenario is gone deliberately.
#
# PR #2 makes `board tui` open Linear mode whenever a herdr space id is present,
# with no fall-through, so `plugin pane open --entrypoint board` no longer reaches
# the kanban board at all. The detail-view `o` jump this scenario used to drive is
# not a surface a person can get to from a pane any more, and a test that unset the
# space id to reach it would be testing a path nobody can take. The kanban board is
# still reachable from a shell with no space id; only the pane route is closed.
#
# What survives is the CLI jump, which is unaffected by the mode decision.

step "Focus the same run through the canonical CLI"
cli_focus_json="$(e2e_board_herdr_mutate -- card run focus "$CARD_ID" "$TARGET_RUN" --json)"
cli_focus_pane="$(printf '%s' "$cli_focus_json" | jget pane_id)"
[ "$cli_focus_pane" = "$TARGET_PANE" ] \
  || fail "CLI focus returned pane '$cli_focus_pane' (expected owned pane '$TARGET_PANE')"
cli_focus_run="$(printf '%s' "$cli_focus_json" | jget run_id)"
[ "$cli_focus_run" = "$TARGET_RUN" ] \
  || fail "CLI focus returned run '$cli_focus_run' (expected requested run '$TARGET_RUN')"
cli_focused=""
for _ in $(seq 1 60); do
  panes="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" 2>/dev/null || true)"
  cli_focused="$(printf '%s' "$panes" | python3 -c '
import json,sys
try: ps=json.load(sys.stdin).get("panes",[])
except Exception: sys.exit(0)
for p in ps:
    if p.get("focused"):
        print(p.get("pane_id", "")); break
' 2>/dev/null || true)"
  [ "$cli_focused" = "$TARGET_PANE" ] && break
  sleep .1
done
[ "$cli_focused" = "$TARGET_PANE" ] \
  || fail "CLI focus left pane '$cli_focused' focused (expected '$TARGET_PANE')"
ok "board card run focus reached run $TARGET_RUN on owned pane $TARGET_PANE"


step "13-jump-to-pane: ALL CHECKS PASSED"
