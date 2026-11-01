#!/bin/sh
# Did main's CI test this commit on every platform?
#
#   ci-release-platforms.sh <sha> [--wait SECONDS]
#
# A pull request tests macOS, relais's primary platform. Ubuntu and Windows
# are tested by the push to main (decisions ADR-0041, `ci.platform-schedule`).
# So nothing is released until the CI `test` job has passed for this exact
# commit on all three platforms, in a run of the PUSH TO MAIN: a pull
# request's run of the same commit does not count, because it may have
# deferred the other legs. The release PR's merge starts main's CI while the
# tag's release is already running, so this polls every 60 s, up to --wait
# (default 0: ask once).
#
# holds-until: a signed run is proven on main; then flip ATTEST_CHECK to 1 in release.yaml
#
# Until then the evidence is the jobs of the push-to-main workflow runs for
# the commit: `test (<os>)` with conclusion `success`. A leg whose suite an
# attestation skipped still concludes `success`.
#
# JOBS_JSON names a file holding {"jobs": [...]} in place of the API, which
# the tests use. A failed `gh` call prints a ::warning:: with its reason and
# counts as "nothing passed yet", so the wait goes on and the final failure
# says why. Exit 0 when every leg passed; exit 1 naming each one still
# missing.
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

# The jobs of every push-to-main run for the commit, as one {"jobs": [...]}.
fetch_jobs() {
  if [ -n "${JOBS_JSON:-}" ]; then
    cat "$JOBS_JSON"
    return
  fi
  repo=${GITHUB_REPOSITORY:?}
  if ! runs=$(gh api "repos/$repo/actions/runs?head_sha=$sha&event=push&branch=main&per_page=100" --jq '.workflow_runs[].id'); then
    echo "::warning::check-runs query failed: could not list the push-to-main runs for $sha" >&2
    echo '{"jobs": []}'
    return
  fi
  for run in $runs; do
    if ! gh api --paginate "repos/$repo/actions/runs/$run/jobs?per_page=100" --jq '{jobs: [.jobs[] | {name, conclusion}]}'; then
      echo "::warning::check-runs query failed: could not read the jobs of run $run" >&2
    fi
  done | jq -s '{jobs: (map(.jobs) | add // [])}'
}

# Attested: for `pre-push-cargo-test` on each platform, a verified signature for
# this exact tree, read with attest's verifier (the same read amont's
# ci-release-gates.sh makes). Off until ATTEST_CHECK=1, so this change can land
# before the first signed run exists; release.yaml turns it on after that proof.
attested() {
  [ "${ATTEST_CHECK:-0}" = 1 ] || return 0
  bin=${GIT_ATTEST:-git-attest}
  missing_att=
  for p in x86_64-linux aarch64-macos x86_64-windows; do
    # stderr is not hidden: a failed verifier call says why in the log.
    if ! "$bin" covered --platform "$p" | tr ' ' '\n' | grep -qx 'pre-push-cargo-test'; then
      missing_att="$missing_att
  $p pre-push-cargo-test (attested)"
    fi
  done
  [ -z "$missing_att" ] || { missing="$missing$missing_att"; return 1; }
  return 0
}

check() {
  jobs=$(fetch_jobs)
  if ! have=$(printf '%s' "$jobs" | jq -r '.jobs[]? | select(.conclusion == "success") | .name'); then
    echo "::warning::check-runs query failed: the answer was not readable JSON" >&2
    have=
  fi
  missing=
  while IFS= read -r leg; do
    if ! printf '%s\n' "$have" | grep -qxF "$leg"; then missing="$missing
  $leg"; fi
  done << EOF
$required
EOF
  attested || true
  [ -z "$missing" ]
}

started=$(date +%s)
while :; do
  if check; then
    echo "ci-release-platforms: $sha passed test on macOS, Ubuntu and Windows on main"
    exit 0
  fi
  elapsed=$(( $(date +%s) - started ))
  if [ "$elapsed" -ge "$wait" ]; then
    echo "ci-release-platforms: $sha has not passed on main, after ${elapsed}s:$missing" >&2
    echo "ci-release-platforms: let main's CI finish on this commit (or re-run it), then re-run this release" >&2
    exit 1
  fi
  echo "ci-release-platforms: waiting for$(printf '%s' "$missing" | tr '\n' ' ') (${elapsed}s of ${wait}s)"
  sleep 60
done
