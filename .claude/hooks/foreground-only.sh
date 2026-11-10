#!/bin/sh
# PreToolUse hook: every command runs in the foreground. Agents that put a
# command in the background, or waited on one with Monitor, sometimes never
# woke up again (M5): the wait outlived the command, or its condition never
# came true. Exit code 2 refuses the call and tells the agent why.
input=$(cat)

if printf '%s' "$input" | grep -q '"tool_name"[[:space:]]*:[[:space:]]*"Monitor"'; then
    echo "Monitor is disabled in this project: run the command in the foreground instead, with a timeout of 600000 ms; the quality gate takes 4 to 6 minutes." >&2
    exit 2
fi

if printf '%s' "$input" | grep -q '"run_in_background"[[:space:]]*:[[:space:]]*true'; then
    echo "Background commands are disabled in this project: run it in the foreground with a timeout of 600000 ms (the quality gate takes 4 to 6 minutes). Do not poll or wait in a loop." >&2
    exit 2
fi

exit 0
