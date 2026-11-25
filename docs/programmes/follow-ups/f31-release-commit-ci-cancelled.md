+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# A push to main does not cancel the release commit's CI

## Question

`ci.yaml` sets `concurrency: ci-${{ github.ref }}` with
`cancel-in-progress: true`. On main, every push cancels the run of the
commit before it. `release.yaml` requires main's CI to pass on the tagged
commit (`scripts/ci-release-platforms.sh`, 2400 s wait), so a release
tagged on a commit that is no longer main's head fails when someone lands
in between. On 2026-11-25, v0.86.0 (098fa32) failed this way twice: #594
landed, its run cancelled the macOS test, and a rerun was cancelled again.
It passed only after a rerun once main was quiet.

Should main's runs never cancel (`cancel-in-progress` only for pull
requests), or should the release check accept a newer main run that
contains the commit?

## Look at

- `.github/workflows/ci.yaml:27-29`: the concurrency block.
- `scripts/ci-release-platforms.sh`: what it waits for.
- Run 45647017193 (attempts 1–3) and release run 45647151143.

## Answer looks like

A CI change so that a push to main never cancels another main run (for
example `cancel-in-progress: ${{ github.event_name == 'pull_request' }}`),
checked by two quick pushes to main whose runs both complete, or a reason
why releases must tolerate it.
