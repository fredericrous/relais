#!/bin/sh
# Does this pull request change something that behaves per platform?
#
#   ci-platform-touched.sh <base-rev>
#
# Prints `true` or `false` on the last line of stdout, after one reason line
# per platform-specific file. `true` makes the pull request run Ubuntu and
# Windows as well as macOS, relais's primary platform (decisions ADR-0041,
# `ci.platform-schedule`). `false` defers them to the push to main, which
# the release waits for.
#
# Copied from amont's scripts/ci-platform-touched.sh, with relais's paths.
# A file counts as platform-specific when it is one of these:
# - what CI and the release build from: the workflows, the scripts, the
#   toolchain, the manifests and lockfile, the Makefile;
# - what ships per platform: the installers;
# - a product source file that compiles differently per platform:
#   `cfg(windows)`, `cfg(unix)`, `cfg(target_os`, `cfg(target_family`, and
#   their `not(`, `any(` and `all(`, in the attribute form (`#[cfg(...)]`,
#   `cfg_attr`) and the macro form (`cfg!(windows)`). This catches `ipc.rs`,
#   `procs.rs` and `tooling.rs`.
# - a product source file the change deleted: it cannot be inspected, so it
#   counts (fail-open).
# A test that is merely `cfg(unix)` (crates/*/tests) does not count: it gates
# where a test runs, not what the product does.
#
# Fail-open: a base it cannot diff against reads as `true`, which runs more.
set -eu

base=${1:?usage: ci-platform-touched.sh <base-rev>}

if ! changed=$(git diff --name-only "$base"...HEAD 2> /dev/null); then
  echo "cannot diff against $base"
  echo true
  exit 0
fi

touched=false
# Any platform keyword inside the parentheses of `cfg(`, `cfg_attr(` or
# `cfg!(`, however nested (`cfg(all(test, unix))`). A keyword inside a string
# (`feature = "windows-sys"`) also matches: that errs toward running more.
cfg='cfg(_attr)?!?\([^)]*(windows|unix|target_os|target_family)'
for f in $changed; do
  why=
  case $f in
    .github/workflows/* | scripts/* | Makefile | Cargo.toml | Cargo.lock \
      | */Cargo.toml | rust-toolchain.toml | .cargo/* | install/*)
      why="builds or ships per platform" ;;
    crates/*/src/*)
      if [ ! -f "$f" ]; then
        why="deleted, cannot inspect"
      elif grep -qE "$cfg" "$f"; then
        why="compiles per platform"
      fi ;;
  esac
  if [ -n "$why" ]; then
    echo "$f: $why"
    touched=true
  fi
done
echo "$touched"
