#!/bin/sh
# SubagentStop hook for the implementer: it may not report while the
# quality gate fails. Exit code 2 sends the failure back to the agent and
# keeps it working. When the agent was already sent back once
# (`stop_hook_active`), it is let go, so a step it must stop on (a question
# for the user) cannot loop; the gate's failure then reaches the reviewer.
input=$(cat)
cd "${CLAUDE_PROJECT_DIR:-$(dirname "$0")/../..}" || exit 0

if output=$(sh scripts/check.sh 2>&1); then
    exit 0
fi

if printf '%s' "$input" | grep -q '"stop_hook_active"[[:space:]]*:[[:space:]]*true'; then
    echo "The quality gate still fails; say so in your report." >&2
    exit 0
fi

{
    echo "The quality gate fails (scripts/check.sh). Fix it before reporting,"
    echo "or report why you cannot. Last lines of its output:"
    echo
    printf '%s\n' "$output" | tail -n 60
} >&2
exit 2
