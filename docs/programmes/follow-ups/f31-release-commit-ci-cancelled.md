+++
type = "task"
mode = "afk"
status = "closed"
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
in between. On 2026-11-25, v0.86.0's release (098fa32) failed this way:
#594 landed and its run cancelled the macOS test on 098fa32, and a rerun
was cancelled again. The release passed on its second attempt, after a
third CI attempt on a quiet main.

Should main's pushes stop sharing one concurrency group, or should the
release check accept a newer main run that contains the commit? Turning
`cancel-in-progress` off is not enough: a group still holds one running
and one pending run, and a newer run cancels the pending one.

## Look at

- `.github/workflows/ci.yaml:27-29`: the concurrency block.
- `scripts/ci-release-platforms.sh`: what it waits for.
- Run 45647017193 (attempts 1–3) and release run 45647151143.

## Answer looks like

A CI change so that a push to main never cancels another main run, for
example a group keyed by the sha on push
(`group: ci-${{ github.event_name == 'pull_request' && github.ref || github.sha }}`),
checked by three quick pushes to main whose runs all complete; or a reason
why releases must tolerate it.

## Outcome

2026-11-25, #602: `ci.yaml` groups a pull request's run by its ref and every
other run (push, schedule) by its sha, and `cancel-in-progress` is on only for
a pull request. A push to main no longer shares a group with the push before
it. #602's own merge started main's run 45715376530 on dc80200. Pushes to main
go through pull requests here, so the three quick pushes were not made: the
check is that the next merge to main while that run is still going leaves it
running to its end.
