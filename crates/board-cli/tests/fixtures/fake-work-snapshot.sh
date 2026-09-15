#!/usr/bin/env bash
# Stand-in for the work plugin's bin/work-snapshot.sh, driven by FAKE_WORK_SNAPSHOT_*.
# The daemon builds the child environment from scratch, so the knobs reach this
# script only through the wrapper support.rs generates (it exports them, then
# execs this file).
set -uo pipefail

space="${1:-}"
if [ -z "$space" ] || ! [[ "$space" =~ ^[A-Za-z0-9_:-]+$ ]]; then
  echo "work-snapshot: refused space id" >&2
  exit 2
fi

if [ -n "${FAKE_WORK_SNAPSHOT_ENV_FILE:-}" ]; then
  env > "$FAKE_WORK_SNAPSHOT_ENV_FILE"
fi

if [ -n "${FAKE_WORK_SNAPSHOT_SLEEP:-}" ]; then
  sleep "$FAKE_WORK_SNAPSHOT_SLEEP"
fi

if [ -n "${FAKE_WORK_SNAPSHOT_EXIT:-}" ]; then
  if [ -n "${FAKE_WORK_SNAPSHOT_STDERR:-}" ]; then
    echo "$FAKE_WORK_SNAPSHOT_STDERR" >&2
  fi
  exit "$FAKE_WORK_SNAPSHOT_EXIT"
fi

here="$(cd "$(dirname "$0")" && pwd)"
fixture_dir="${FAKE_WORK_SNAPSHOT_FIXTURE_DIR:-$here/../../../board-core/tests/fixtures/linear-snapshot}"
cat "$fixture_dir/${FAKE_WORK_SNAPSHOT_FIXTURE:-bound-with-view}.json"
