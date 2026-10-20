#!/bin/sh
# Runs `claude plugin test` in claude-plugin/. Claude Code's runner has a fixed
# 5000 ms limit per test, so a loaded machine fails tests that are fine. When
# EVERY failure is that timeout, rerun the whole suite once and exit with the
# rerun's status. Any other failure exits non-zero at once, with no rerun.
#
# One retry owner per execution context: under relais (RELAIS_CHECK is set in
# every check's environment) the check's own `retry` owns the rerun, so this
# script never reruns; CI, without it, keeps the rerun.
#
# CLAUDE_BIN overrides the command (used by the script's test).
set -u

root=$(cd "$(dirname "$0")/.." && pwd)
claude_bin=${CLAUDE_BIN:-claude}
out=$(mktemp "${TMPDIR:-/tmp}/plugin-test.XXXXXX") || exit 1
trap 'rm -f "$out"' EXIT HUP INT TERM

cd "$root/claude-plugin" || exit 1

if "$claude_bin" plugin test >"$out" 2>&1; then
	cat "$out"
	exit 0
fi
cat "$out"

# bun test prints "(fail) <name>" per failing test and "error: <message>" for
# the cause. Rerun only when there is at least one failing test, no error line
# other than the timeout, and a timeout for every failing test.
failed=$(grep -c '^(fail) ' "$out")
timed=$(grep -c 'timed out after 5000 ms' "$out")
other=$(grep -i -E '^[[:space:]]*([a-z]*error|fatal)[:[:space:]]' "$out" | grep -v -c 'timed out after 5000 ms')

if [ -z "${RELAIS_CHECK:-}" ] && [ "$failed" -gt 0 ] && [ "$timed" -ge "$failed" ] && [ "$other" -eq 0 ]; then
	echo "plugin-test: every failure was 'timed out after 5000 ms' (machine load); rerunning the suite once" >&2
	"$claude_bin" plugin test
	exit $?
fi
exit 1
