#!/usr/bin/env bash
# Stand-in for the work plugin's bin/work-{spaces,projects,views}.sh, driven by
# FAKE_WORK_LIST_*. The wrapper support.rs generates passes the kind first.
set -uo pipefail

kind="${1:-}"
shift || true

if [ -n "${FAKE_WORK_LIST_EXIT:-}" ]; then
  exit "$FAKE_WORK_LIST_EXIT"
fi

if [ -n "${FAKE_WORK_LIST_JSON:-}" ]; then
  printf '%s\n' "$FAKE_WORK_LIST_JSON"
  exit 0
fi

case "$kind" in
  spaces)
    # Like the real script: herdr is the source of every space row.
    if [ -z "${HERDR_SOCKET_PATH:-}" ]; then
      printf '%s\n' '{"status":"unavailable","message":"herdr socket missing","rows":[]}'
    else
      printf '%s\n' '{"status":"ok","message":null,"rows":[{"id":"wA","label":"alpha","live":true,"state":"bound","project_id":"proj-1","project_name":"Example"},{"id":"wB","label":"beta","live":true,"state":"unbound","project_id":null,"project_name":null}]}'
    fi
    ;;
  projects)
    printf '%s\n' '{"status":"ok","message":null,"rows":[{"id":"proj-1","name":"Example","team_key":"EX"},{"id":"proj-2","name":"Sample","team_key":"SA"}]}'
    ;;
  views)
    project="${1:-}"
    printf '{"status":"ok","message":null,"rows":[{"id":"view-1","name":"Open work in %s"}]}\n' "$project"
    ;;
  *)
    echo "fake-work-list: unknown kind" >&2
    exit 2
    ;;
esac
