#!/usr/bin/env bash
# Did this branch delete a test whose bugs nothing else catches?
#
#   scripts/mutants-check.sh [--base REV] FILE...
#
# The bar of decisions' plan 2026-10-17-amont-tests-follow-user-flows: a
# test may go only if every mutant it caught is still caught by the tests
# that remain. Checked exactly, and cheaply, in two runs of cargo-mutants
# over FILE... (the product files the deleted tests exercise):
#
#   1. BEFORE, on REV (default origin/main): each mutant against ONLY the
#      tests this branch deletes. What they catch is the set at risk.
#   2. AFTER, on the working tree: ONLY that set, against the whole
#      remaining suite. Every one must be caught again.
#
# A test is "deleted" when its name is listed by `cargo test -- --list` on
# REV and not here, so a test turned into a table row, or moved into a flow
# under a new name, is checked like a deleted one.
#
# Mutants are matched across the two trees by file, description and
# occurrence, not by line: a table conversion moves lines in the same file.
#
# Exit 0: nothing deleted, or nothing lost. Exit 1: a mutant the deleted
# tests caught is missed, or times out, or no longer exists, each listed.
# Exit 2: a usage error, or a run that could not start.
#
# Needs cargo-mutants on PATH. MUTANTS_ARGS adds flags to both runs (CI
# passes `-j 2`), MUTANTS_TIMEOUT sets each mutant's test timeout (600 s);
# MUTANTS_TEST_ARGS adds arguments for the test binaries
# (a machine missing a tool skips that test with `--skip NAME`).
# MUTANTS_SHARD=k/n (k from 0) checks only every n-th mutant of run 1,
# starting at k, and so only its share of run 2: n machines, one shard
# each, check the whole set between them.
set -euo pipefail

base=origin/main
files=()
while [ $# -gt 0 ]; do
    case $1 in
        --base) base=${2:?--base wants a revision}; shift 2 ;;
        -h | --help) awk 'NR > 1 && !/^#/ {exit} NR > 1' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) echo "mutants-check: unknown flag $1" >&2; exit 2 ;;
        *) files+=("$1"); shift ;;
    esac
done
[ ${#files[@]} -gt 0 ] || { echo "mutants-check: name the product files the deleted tests exercise" >&2; exit 2; }
command -v cargo-mutants > /dev/null || { echo "mutants-check: cargo-mutants is not installed" >&2; exit 2; }

root=$(git rev-parse --show-toplevel)
work=$(mktemp -d)
trap 'git -C "$root" worktree remove --force "$work/base" > /dev/null 2>&1 || true; rm -rf "$work"' EXIT

# Every test cargo would run here, one per line as `package<TAB>name`.
list_tests() { # dir target-dir
    local pkg
    for pkg in $(cd "$1" && cargo metadata --no-deps --format-version 1 \
        | python3 -c 'import json,sys; print(" ".join(p["name"] for p in json.load(sys.stdin)["packages"]))'); do
        (cd "$1" && CARGO_TARGET_DIR=$2 cargo test -q -p "$pkg" -- --list 2> /dev/null) \
            | sed -n "s/: test\$//p" | sed "s/^/$pkg\t/"
    done | sort -u
}

# mutants.json of a run -> "file<TAB>description<TAB>n" keys of the given
# outcome (or every mutant, for "all"), n counting identical descriptions
# in one file in source order.
keys() { # outcomes-dir outcome
    python3 - "$1" "$2" << 'EOF'
import json, os, re, sys
out, want = sys.argv[1], sys.argv[2]
names = []
if want == "all":
    names = [l.strip() for l in open(os.path.join(out, "listed.txt")) if l.strip()]
else:
    path = os.path.join(out, "mutants.out", want + ".txt")
    names = [l.strip() for l in open(path)] if os.path.exists(path) else []
    order = [l.strip() for l in open(os.path.join(out, "listed.txt")) if l.strip()]
    rank = {n: i for i, n in enumerate(order)}
    names.sort(key=lambda n: rank.get(n, 1 << 30))
seen = {}
pat = re.compile(r"^(.*?):\d+:\d+: (.*)$")
everything = [l.strip() for l in open(os.path.join(out, "listed.txt")) if l.strip()]
index = {}
for n in everything:
    m = pat.match(n)
    k = (m.group(1), m.group(2))
    seen[k] = seen.get(k, 0) + 1
    index[n] = (m.group(1), m.group(2), seen[k])
for n in names:
    f, d, i = index[n]
    print(f"{f}\t{d}\t{i}\t{n}")
EOF
}

# A regex matching exactly these mutant names.
exact_re() {
    python3 -c 'import re,sys; print("^(" + "|".join(re.escape(l.rstrip("\n")) for l in sys.stdin if l.strip()) + ")$")'
}

echo "--- tests on $base and here"
git -C "$root" worktree add -q --detach "$work/base" "$base"
list_tests "$work/base" "$root/target/mutants-base" > "$work/base-tests"
list_tests "$root" "$root/target" > "$work/head-tests"
comm -23 "$work/base-tests" "$work/head-tests" > "$work/deleted-with-package"
cut -f2 "$work/deleted-with-package" | sort -u > "$work/deleted"
echo "    $(wc -l < "$work/base-tests") on $base, $(wc -l < "$work/head-tests") here, $(wc -l < "$work/deleted") deleted or renamed"
if [ ! -s "$work/deleted" ]; then
    echo "mutants-check: no test was deleted; nothing to check"
    exit 0
fi

file_args=()
for f in "${files[@]}"; do file_args+=(--file "$f"); done
read -r -a extra <<< "${MUTANTS_ARGS:-}"
# The whole suite takes about 100 s here; cargo-mutants' automatic timeout,
# scaled from a filtered baseline, would read every mutant as a timeout.
timeout=${MUTANTS_TIMEOUT:-600}
read -r -a test_extra <<< "${MUTANTS_TEST_ARGS:-}"

filters=()
while read -r t; do filters+=("$t"); done < "$work/deleted"
echo "--- 1. on $base: each mutant of ${files[*]} against the deleted tests only"
mkdir -p "$work/before"
(cd "$work/base" && cargo mutants --list "${file_args[@]}") > "$work/before/listed.txt"

# Only the mutants on a line the deleted tests execute: a test cannot catch
# a change to a line it never runs, and each mutant costs a build. One
# coverage run of the deleted tests (cargo-llvm-cov, which follows the
# binaries they spawn) says which lines those are. Without cargo-llvm-cov,
# every mutant is tried, as before.
only=()
if command -v cargo-llvm-cov > /dev/null; then
    pkgs=()
    while read -r p; do pkgs+=(-p "$p"); done < <(cut -f1 "$work/deleted-with-package" | sort -u)
    # Collect, then report over the whole workspace: a report scoped to the
    # packages that hold the tests drops every other package's lines, and
    # an integration test's lines are mostly amont-runtime's, run by the
    # binary it spawns. Scoped, every runtime mutant read as unreached and
    # the check passed without trying one.
    (cd "$work/base" && cargo llvm-cov --no-report "${pkgs[@]}" \
        -- --exact "${filters[@]}" "${test_extra[@]}" \
        && cargo llvm-cov report --lcov --output-path "$work/cov.lcov") > "$work/cov.log" 2>&1 || true
    if [ -s "$work/cov.lcov" ]; then
        python3 - "$work/cov.lcov" "$work/base" "$work/before/listed.txt" "$work/covered-mutants" << 'EOF'
import os, re, sys
lcov, base, listed, out = sys.argv[1:]
hit, cur = {}, None
for l in open(lcov):
    l = l.strip()
    if l.startswith("SF:"):
        cur = os.path.relpath(l[3:], base)
    elif l.startswith("DA:") and cur:
        n, c = l[3:].split(",")[:2]
        if int(c) > 0:
            hit.setdefault(cur, set()).add(int(n))
pat = re.compile(r"^(.*?):(\d+):\d+: ")
with open(out, "w") as o:
    for name in open(listed):
        m = pat.match(name)
        if m and int(m.group(2)) in hit.get(m.group(1), ()):
            o.write(name)
EOF
        echo "    $(wc -l < "$work/covered-mutants") of $(wc -l < "$work/before/listed.txt") mutants are on lines the deleted tests run"
        if [ ! -s "$work/covered-mutants" ]; then
            echo "mutants-check: the deleted tests run none of these lines; nothing is lost"
            exit 0
        fi
        only=(--re "$(exact_re < "$work/covered-mutants")")
    else
        echo "    (coverage did not run; trying every mutant)"
        tail -5 "$work/cov.log" | sed 's/^/      /'
    fi
else
    echo "    (cargo-llvm-cov is not installed; trying every mutant)"
fi
# Only the packages that hold a deleted test are built and run for each
# mutant: the rest cannot catch anything here, and building them is most
# of a mutant's cost. The automatic timeout fits the few tests that run.
packages=()
while read -r p; do packages+=(--test-package "$p"); done < <(cut -f1 "$work/deleted-with-package" | sort -u)
shard=()
[ -n "${MUTANTS_SHARD:-}" ] && shard=(--shard "$MUTANTS_SHARD" --sharding round-robin)
echo "    building and running ${packages[*]//--test-package /} only"
(cd "$work/base" && cargo mutants \
    "${packages[@]}" "${only[@]}" "${shard[@]}" --no-shuffle "${file_args[@]}" "${extra[@]}" \
    --output "$work/before" -- -- --exact "${filters[@]}" "${test_extra[@]}") \
    > "$work/before/run.log" 2>&1 || true
if grep -q "No mutants found" "$work/before/run.log"; then
    echo "mutants-check: no mutant of these files in this shard; nothing is lost"
    exit 0
fi
# cargo-mutants ends with "N mutants tested", or "1 mutant tested".
grep -Eq "mutants? tested" "$work/before/run.log" || { tail -20 "$work/before/run.log" >&2; echo "mutants-check: the run on $base did not complete" >&2; exit 2; }
keys "$work/before" caught > "$work/at-risk"
echo "    the deleted tests catch $(wc -l < "$work/at-risk") of $(wc -l < "$work/before/listed.txt") mutants:"
cut -f4 "$work/at-risk" | sed 's/^/      /'
if [ ! -s "$work/at-risk" ]; then
    echo "mutants-check: the deleted tests caught no mutant in these files; nothing is lost"
    exit 0
fi

echo "--- 2. here: those mutants against the whole remaining suite"
mkdir -p "$work/after"
cargo mutants --list "${file_args[@]}" > "$work/after/listed.txt"
keys "$work/after" all > "$work/head-keys"
# at-risk keys -> this tree's names, or a line saying the mutant is gone.
python3 - "$work/at-risk" "$work/head-keys" "$work/targets" "$work/gone" << 'EOF'
import sys
risk, head, targets, gone = sys.argv[1:]
here = {}
for l in open(head):
    f, d, i, n = l.rstrip("\n").split("\t")
    here[(f, d, i)] = n
with open(targets, "w") as t, open(gone, "w") as g:
    for l in open(risk):
        f, d, i, n = l.rstrip("\n").split("\t")
        if (f, d, i) in here:
            t.write(here[(f, d, i)] + "\n")
        else:
            g.write(n + "\n")
EOF
re=$(exact_re < "$work/targets")
cargo mutants --test-workspace=true --no-shuffle --timeout "$timeout" "${file_args[@]}" "${extra[@]}" \
    --re "$re" --output "$work/after" -- -- "${test_extra[@]}" \
    > "$work/after/run.log" 2>&1 || true
grep -Eq "mutants? tested" "$work/after/run.log" || { tail -20 "$work/after/run.log" >&2; echo "mutants-check: the run here did not complete" >&2; exit 2; }

# Under GitHub Actions each lost mutant is also an error annotation: the
# job log is not always readable where the result is wanted.
annotate() { # why < names
    [ "${GITHUB_ACTIONS:-}" = true ] || { cat > /dev/null; return; }
    while read -r m; do printf '::error title=mutant %s::%s\n' "$1" "$m"; done
}
# Only the mutants asked for count. cargo-mutants 27.1.0 does not apply
# --re to "delete field ... from struct ... expression" mutants: every one
# in the file runs in step 2 whether or not it was at risk, and one no
# test catches would read as lost (amont#370).
lost=0
for outcome in caught missed timeout; do
    f="$work/after/mutants.out/$outcome.txt"
    [ -f "$f" ] || continue
    grep -Fx -f "$work/targets" "$f" > "$f.asked" || true
    mv "$f.asked" "$f"
done
for outcome in missed timeout; do
    f="$work/after/mutants.out/$outcome.txt"
    if [ -s "$f" ]; then
        echo "mutants-check: $outcome here, caught on $base by a deleted test:"
        sed 's/^/    /' "$f"
        annotate "$outcome" < "$f"
        lost=1
    fi
done
if [ -s "$work/gone" ]; then
    echo "mutants-check: caught on $base, and no longer a mutant here (the product code changed):"
    sed 's/^/    /' "$work/gone"
    annotate gone < "$work/gone"
    lost=1
fi
caught=$(wc -l < "$work/after/mutants.out/caught.txt" 2> /dev/null || echo 0)
echo "    $caught of $(wc -l < "$work/targets") still caught"
[ "$lost" = 0 ] && echo "mutants-check: nothing lost"
exit "$lost"
