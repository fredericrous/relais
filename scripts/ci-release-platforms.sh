#!/bin/sh
# Did main's CI test this commit on every platform?
#
#   ci-release-platforms.sh <sha> [--wait SECONDS]
#
# A pull request tests macOS, relais's primary platform. Ubuntu and Windows
# are tested by the push to main (decisions ADR-0041, `ci.platform-schedule`).
# So nothing is released until the CI `test` job has passed for this exact
# commit on all three platforms. The release PR's merge starts main's CI
# while the tag's release is already running, so this polls every 60 s, up
# to --wait (default 0: ask once).
#
# Until relais's CI signs attestations (plan phase C2), the evidence is the
# check runs GitHub records for the commit: `test (<os>)` with conclusion
# `success`. A leg whose suite an attestation skipped still concludes
# `success`. C2 switches this to the signed attestations, as amont's
# ci-release-gates.sh asks git-attest.
#
# CHECK_RUNS_JSON names a file holding the API's answer, which the tests use
# in place of `gh api`. Exit 0 when every leg passed; exit 1 naming each one
# still missing.
set -eu

sha=${1:?usage: ci-release-platforms.sh <sha> [--wait SECONDS]}
shift
wait=0
while [ $# -gt 0 ]; do
  case $1 in
    --wait) wait=${2:?--wait wants seconds}; shift 2 ;;
    *) echo "ci-release-platforms: unknown argument $1" >&2; exit 2 ;;
  esac
done

required='test (macos-latest)
test (ubuntu-latest)
test (windows-latest)'

# The names of the check runs that concluded `success` for the commit.
passed() {
  if [ -n "${CHECK_RUNS_JSON:-}" ]; then
    cat "$CHECK_RUNS_JSON"
  else
    gh api --paginate "repos/${GITHUB_REPOSITORY:?}/commits/$sha/check-runs?per_page=100" 2> /dev/null || true
  fi | jq -r '.check_runs[]? | select(.conclusion == "success") | .name' 2> /dev/null || true
}

check() {
  have=$(passed)
  missing=
  while IFS= read -r leg; do
    if ! printf '%s\n' "$have" | grep -qxF "$leg"; then missing="$missing
  $leg"; fi
  done << EOF
$required
EOF
  [ -z "$missing" ]
}

started=$(date +%s)
while :; do
  if check; then
    echo "ci-release-platforms: $sha passed test on macOS, Ubuntu and Windows"
    exit 0
  fi
  elapsed=$(( $(date +%s) - started ))
  if [ "$elapsed" -ge "$wait" ]; then
    echo "ci-release-platforms: $sha has not passed, after ${elapsed}s:$missing" >&2
    echo "ci-release-platforms: let main's CI finish on this commit (or re-run it), then re-run this release" >&2
    exit 1
  fi
  echo "ci-release-platforms: waiting for$(printf '%s' "$missing" | tr '\n' ' ') (${elapsed}s of ${wait}s)"
  sleep 60
done
