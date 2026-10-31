#!/bin/sh
# Which platforms this CI run tests, and what it says about the others.
#
#   EVENT=pull_request PLATFORM_CODE=false FULL_CI=false ci-platform-plan.sh
#
# macOS is relais's primary platform (decisions ADR-0041 and the programme
# ticket t08): every run tests it. Ubuntu and Windows run too, except on a
# pull request that touches nothing platform-specific (PLATFORM_CODE, from
# ci-platform-touched.sh) and does not carry the `full-ci` label (FULL_CI).
# Those legs are DEFERRED to the push to main, which tests all three for the
# same tree, and the release waits for that (release.yaml, `platforms`).
#
# Writes to $GITHUB_OUTPUT (stdout when unset):
#   run_other     true when the Ubuntu and Windows legs run here
#   test_matrix   JSON for `strategy.matrix.include`: {os, platform}
# and says what was deferred, in the log and in $GITHUB_STEP_SUMMARY, so a
# missing leg is never a silent absence (`ci.a-deferred-job-says-so`).
#
# Fail-open: an unset or unreadable PLATFORM_CODE reads as `true`, which runs
# more. `push` and `schedule` always run every platform.
set -eu

event=${EVENT:-}
platform_code=${PLATFORM_CODE:-true}
full_ci=${FULL_CI:-false}

out=${GITHUB_OUTPUT:-/dev/stdout}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

say() {
  echo "$1"
  echo "$1" >> "$summary"
}

macos='{"os":"macos-latest","platform":"aarch64-macos"}'
others='{"os":"ubuntu-latest","platform":"x86_64-linux"},{"os":"windows-latest","platform":"x86_64-windows"}'

if [ "$event" = pull_request ] && [ "$platform_code" = false ] && [ "$full_ci" != true ]; then
  run_other=false
  say "test (macos-latest): running, macOS is the primary platform"
  say "test (ubuntu-latest), test (windows-latest): deferred to the push to main, which tests them for this tree before any release (label full-ci runs them here)"
  matrix="[$macos]"
else
  run_other=true
  if [ "$event" = pull_request ]; then
    if [ "$full_ci" = true ]; then why="the full-ci label"; else why="platform-specific changes"; fi
    say "test (macos, ubuntu, windows): running, this pull request carries $why"
  else
    say "test (macos, ubuntu, windows): running, a $event run tests every platform"
  fi
  matrix="[$macos,$others]"
fi

{
  echo "run_other=$run_other"
  echo "test_matrix=$matrix"
} >> "$out"
