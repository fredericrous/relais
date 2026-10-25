#!/usr/bin/env bash
# Did this branch delete a test whose bugs nothing else catches?
#
#   scripts/mutants-check.sh [--base REV] [--manifest FILE] [--equivalents FILE] FILE...
#
# Copied from amont's scripts/mutants-check.sh at b247e39 (2026-10-19) and
# hardened for relais by Phase 4 of decisions'
# docs/plans/2026-10-25-relais-tests-follow-user-flows.md: the check fails
# closed. The pure logic lives in scripts/mutants_check.py, tested by
# scripts/test_mutants_check.py; this script runs git and cargo.
#
# The bar of the plan: a test may go only if every mutant it caught is still
# caught by the tests that remain. Checked exactly, and cheaply, in two runs
# of cargo-mutants over FILE... (the product files the deleted tests
# exercise):
#
#   0. The manifest gate. Affected tests are the tests whose names disappear
#      between the merge base and here, plus the manifest's `test` entries.
#      Every base line `git diff -U0` removes or modifies under crates/ must
#      lie in an affected test's fn or in a manifest `helper` item, or the
#      check exits 2 naming file:line: a changed helper or an assertion
#      removed from a retained test would otherwise go unchecked.
#   1. BEFORE, on the merge base: each mutant against ONLY the affected
#      tests. What they catch is the set at risk.
#   2. AFTER, on the working tree: ONLY that set, against the whole
#      remaining suite. Every one must be caught again.
#
# Mutants are matched across the two trees by file, description and
# occurrence, not by line: a table conversion moves lines in the same file.
# Every requested mutant must appear in exactly one outcome file of
# mutants.out; one that appears in none is `absent`.
#
# The manifest is TSV, one entry per line, `#` comments allowed:
#   test<TAB>package<TAB>name as listed by `cargo test -- --list`<TAB>source file
#   helper<TAB>source file<TAB>item name (fn, mod, struct, enum, const, ...)
# The equivalents file lists base mutant names no test could tell from the
# original: `name<TAB>reason`; a line without a reason is a usage error.
#
# Exit 0: nothing deleted, or nothing lost. Exit 1: a mutant the affected
# tests caught is missed here, or no longer exists. Exit 2: a usage error,
# an invalid or incomplete manifest, `cargo test --list` failing, or
# incomplete evidence (a timeout, an unviable or absent mutant). 2 wins
# over 1.
#
# Needs cargo-mutants on PATH. MUTANTS_ARGS adds flags to both runs (CI
# passes `-j 2`), MUTANTS_TIMEOUT sets each mutant's test timeout (900 s);
# MUTANTS_TEST_ARGS adds arguments for the test binaries
# (a machine missing a tool skips that test with `--skip NAME`).
# MUTANTS_SHARD=k/n (k from 0) checks only every n-th mutant of run 1,
# starting at k, and so only its share of run 2: n machines, one shard
# each, check the whole set between them.
set -euo pipefail

base=origin/main
manifest=
equivalents=
files=()
while [ $# -gt 0 ]; do
    case $1 in
        --base) base=${2:?--base wants a revision}; shift 2 ;;
        --manifest) manifest=${2:?--manifest wants a file}; shift 2 ;;
        --equivalents) equivalents=${2:?--equivalents wants a file}; shift 2 ;;
        -h | --help) awk 'NR > 1 && !/^#/ {exit} NR > 1' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) echo "mutants-check: unknown flag $1" >&2; exit 2 ;;
        *) files+=("$1"); shift ;;
    esac
done
[ ${#files[@]} -gt 0 ] || { echo "mutants-check: name the product files the deleted tests exercise" >&2; exit 2; }
command -v cargo-mutants > /dev/null || { echo "mutants-check: cargo-mutants is not installed" >&2; exit 2; }
command -v python3 > /dev/null || { echo "mutants-check: python3 is not installed" >&2; exit 2; }
for named in "$manifest" "$equivalents"; do
    [ -z "$named" ] || [ -f "$named" ] || { echo "mutants-check: $named is not a file" >&2; exit 2; }
done

here=$(cd "$(dirname "$0")" && pwd)
helper="$here/mutants_check.py"
py() { python3 "$helper" "$@"; }
[ -z "$equivalents" ] || py check-equivalents "$equivalents" || exit 2

root=$(git rev-parse --show-toplevel)
work=$(mktemp -d)
trap 'git -C "$root" worktree remove --force "$work/base" > /dev/null 2>&1 || true; rm -rf "$work"' EXIT
[ -n "$manifest" ] || { : > "$work/manifest.tsv"; manifest=$work/manifest.tsv; }
[ -n "$equivalents" ] || { : > "$work/equivalents.tsv"; equivalents=$work/equivalents.tsv; }

# Every test cargo would run here, one per line as `package<TAB>name`. A
# failing `cargo test -- --list` ends the check with 2 and cargo's stderr.
list_tests() { # dir target-dir
    local pkg packages listing
    packages=$(cd "$1" && cargo metadata --no-deps --format-version 1 \
        | python3 -c 'import json,sys; print(" ".join(p["name"] for p in json.load(sys.stdin)["packages"]))') \
        || { echo "mutants-check: cargo metadata failed in $1" >&2; return 2; }
    for pkg in $packages; do
        if ! listing=$(cd "$1" && CARGO_TARGET_DIR=$2 cargo test -q -p "$pkg" -- --list 2> "$work/list.err"); then
            cat "$work/list.err" >&2
            echo "mutants-check: cargo test -- --list failed for $pkg in $1" >&2
            return 2
        fi
        # awk, not sed: BSD sed writes `\t` as a literal t.
        printf '%s\n' "$listing" | awk -v pkg="$pkg" 'sub(/: test$/, "") { print pkg "\t" $0 }'
    done | sort -u
}

# A regex matching exactly these mutant names.
exact_re() {
    python3 -c 'import re,sys; print("^(" + "|".join(re.escape(l.rstrip("\n")) for l in sys.stdin if l.strip()) + ")$")'
}

echo "--- tests on $base and here"
mb=$(git -C "$root" merge-base "$base" HEAD) || { echo "mutants-check: no merge base of $base and HEAD" >&2; exit 2; }
git -C "$root" worktree add -q --detach "$work/base" "$mb"
list_tests "$work/base" "$root/target/mutants-base" > "$work/base-tests" || exit 2
list_tests "$root" "$root/target" > "$work/head-tests" || exit 2
echo "    $(wc -l < "$work/base-tests") on $base, $(wc -l < "$work/head-tests") here"

echo "--- 0. the manifest covers every base line the diff removes or modifies"
git -C "$root" diff -U0 --no-renames "$mb..HEAD" -- crates/ > "$work/diff"
gate_rc=0
py gate --diff "$work/diff" --base-root "$work/base" --base-tests "$work/base-tests" \
    --head-tests "$work/head-tests" --manifest "$manifest" --affected-out "$work/affected-with-package" || gate_rc=$?
[ "$gate_rc" = 0 ] || exit 2
cut -f2 "$work/affected-with-package" | sort -u > "$work/deleted"
echo "    $(wc -l < "$work/deleted") affected tests (deleted, renamed or in the manifest)"
if [ ! -s "$work/deleted" ]; then
    echo "mutants-check: nothing deleted; nothing to check"
    exit 0
fi

file_args=()
for f in "${files[@]}"; do file_args+=(--file "$f"); done
read -r -a extra <<< "${MUTANTS_ARGS:-}"
# holds-until: the suite grows past what two parallel runs finish in 900 s.
# Basis: the CI ubuntu `cargo test` step took 191 s including the cached
# build (relais#305, 2026-10-25); two suites run at once under `-j 2`, so
# 900 s is about 3x headroom. cargo-mutants' automatic timeout, scaled from
# a filtered baseline, would read every mutant as a timeout.
timeout=${MUTANTS_TIMEOUT:-900}
read -r -a test_extra <<< "${MUTANTS_TEST_ARGS:-}"

filters=()
while read -r t; do filters+=("$t"); done < "$work/deleted"
echo "--- 1. on $base: each mutant of ${files[*]} against the affected tests only"
mkdir -p "$work/before"
(cd "$work/base" && cargo mutants --list "${file_args[@]}") > "$work/before/listed.txt"

# Only the mutants on a line the affected tests execute: a test cannot catch
# a change to a line it never runs, and each mutant costs a build. One
# coverage run of the affected tests (cargo-llvm-cov, which follows the
# binaries they spawn) says which lines those are. Without cargo-llvm-cov,
# every mutant is tried.
candidates="$work/before/listed.txt"
if command -v cargo-llvm-cov > /dev/null; then
    pkgs=()
    while read -r p; do pkgs+=(-p "$p"); done < <(cut -f1 "$work/affected-with-package" | sort -u)
    # Collect, then report over the whole workspace: a report scoped to the
    # packages that hold the tests drops every other package's lines.
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
        echo "    $(wc -l < "$work/covered-mutants") of $(wc -l < "$work/before/listed.txt") mutants are on lines the affected tests run"
        candidates="$work/covered-mutants"
    else
        echo "    (coverage did not run; trying every mutant)"
        tail -5 "$work/cov.log" | sed 's/^/      /'
    fi
else
    echo "    (cargo-llvm-cov is not installed; trying every mutant)"
fi
# Exactly the requested mutants, this shard's share of the candidates.
py shard --in "$candidates" --shard "${MUTANTS_SHARD:-}" --out "$work/requested" || exit 2
if [ ! -s "$work/requested" ]; then
    echo "mutants-check: no mutant of these files to try in this shard; nothing is lost"
    exit 0
fi
only=(--re "$(exact_re < "$work/requested")")
# Only the packages that hold an affected test are built and run for each
# mutant: the rest cannot catch anything here, and building them is most
# of a mutant's cost. The automatic timeout fits the few tests that run.
packages=()
while read -r p; do packages+=(--test-package "$p"); done < <(cut -f1 "$work/affected-with-package" | sort -u)
echo "    building and running ${packages[*]//--test-package /} only"
(cd "$work/base" && cargo mutants \
    "${packages[@]}" "${only[@]}" --no-shuffle "${file_args[@]}" "${extra[@]}" \
    --output "$work/before" -- -- --exact "${filters[@]}" "${test_extra[@]}") \
    > "$work/before/run.log" 2>&1 || true
step1_rc=0
py step1 --requested "$work/requested" --outdir "$work/before" --listed "$work/before/listed.txt" \
    --at-risk-out "$work/at-risk" || step1_rc=$?
if [ "$step1_rc" != 0 ]; then
    tail -20 "$work/before/run.log" >&2
    echo "mutants-check: the run on $base is incomplete" >&2
    exit 2
fi
echo "    the affected tests catch $(wc -l < "$work/at-risk") of $(wc -l < "$work/requested") requested mutants:"
cut -f4 "$work/at-risk" | sed 's/^/      /'
if [ ! -s "$work/at-risk" ]; then
    echo "mutants-check: the affected tests caught no mutant in these files; nothing is lost"
    exit 0
fi

echo "--- 2. here: those mutants against the whole remaining suite"
mkdir -p "$work/after"
cargo mutants --list "${file_args[@]}" > "$work/after/listed.txt"
py plan --at-risk "$work/at-risk" --head-listed "$work/after/listed.txt" --equivalents "$equivalents" \
    --targets-out "$work/targets" --gone-out "$work/gone" --equivalents-out "$work/equivalent" || exit 2
if [ -s "$work/targets" ]; then
    re=$(exact_re < "$work/targets")
    cargo mutants --test-workspace=true --no-shuffle --timeout "$timeout" "${file_args[@]}" "${extra[@]}" \
        --re "$re" --output "$work/after" -- -- "${test_extra[@]}" \
        > "$work/after/run.log" 2>&1 || true
fi
# Reconcile only the mutants asked for: cargo-mutants 27.1.0 does not apply
# --re to "delete field ... from struct ... expression" mutants, so every one
# in the file runs in step 2 whether or not it was at risk (amont#370).
rc=0
py verdict --targets "$work/targets" --gone "$work/gone" --equivalents-out "$work/equivalent" \
    --outdir "$work/after" || rc=$?
if [ "$rc" = 2 ] && [ -f "$work/after/run.log" ]; then
    tail -20 "$work/after/run.log" >&2
fi
exit "$rc"
