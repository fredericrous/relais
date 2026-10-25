#!/usr/bin/env python3
"""The pure logic of scripts/mutants-check.sh.

Copied from amont's mutants-check (b247e39, 2026-10-19) and hardened for
relais by Phase 4 of decisions' plan 2026-10-25-relais-tests-follow-user-flows:
the deletion check fails closed. Manifest parsing and validation, the Rust
item-span scan, diff-hunk mapping, outcome reconciliation, exit-code choice
and mutant key matching live here so that fixtures can test them; the shell
script only runs git and cargo.

Exit codes: 0 nothing lost, 1 a mutant the deleted tests caught is lost,
2 a usage error or evidence that is incomplete.
"""

import argparse
import bisect
import os
import re
import sys
from typing import Callable, Dict, List, NamedTuple, Optional, Set, Tuple

OUTCOME_FILES = ("caught", "missed", "timeout", "unviable")
MODULE_LEVEL = "<module level>"


class UsageError(Exception):
    """An input that cannot be used; the check exits 2."""


# ---------------------------------------------------------------- Rust scan


def mask(source: str) -> str:
    """The source with comments, strings and char literals blanked.

    Offsets and newlines are preserved, so a position in the result is a
    position in the source.
    """
    out = list(source)
    length = len(source)

    def blank(begin: int, end: int) -> None:
        for k in range(begin, end):
            if out[k] != "\n":
                out[k] = " "

    raw_open = re.compile(r"b?r(#*)\"")
    i = 0
    while i < length:
        c = source[i]
        if source.startswith("//", i):
            j = source.find("\n", i)
            j = length if j < 0 else j
            blank(i, j)
            i = j
        elif source.startswith("/*", i):
            depth, j = 1, i + 2
            while j < length and depth:
                if source.startswith("/*", j):
                    depth += 1
                    j += 2
                elif source.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c in "rb" and (i == 0 or not (source[i - 1].isalnum() or source[i - 1] == "_")) and raw_open.match(source, i):
            m = raw_open.match(source, i)
            closing = '"' + m.group(1)
            j = source.find(closing, m.end())
            j = length if j < 0 else j + len(closing)
            blank(i, j)
            i = j
        elif c == '"':
            j = i + 1
            while j < length and source[j] != '"':
                j += 2 if source[j] == "\\" else 1
            j = min(j + 1, length)
            blank(i, j)
            i = j
        elif c == "'":
            if i + 1 < length and source[i + 1] == "\\":
                j = source.find("'", i + 3)
                j = length if j < 0 else j + 1
                blank(i, j)
                i = j
            elif i + 2 < length and source[i + 2] == "'":
                blank(i, i + 3)
                i += 3
            else:
                i += 1  # a lifetime
        else:
            i += 1
    return "".join(out)


class Item(NamedTuple):
    kind: str
    name: str
    aliases: frozenset
    start: int  # 1-based line, attributes and doc comments included
    end: int  # 1-based line, inclusive


_ITEM = re.compile(
    r"(?<![\w])(?:(fn|mod|struct|enum|trait|static|const)\s+(?:mut\s+)?(?!(?:fn|unsafe|async|extern)\b)(\w+)"
    r"|(macro_rules)\s*!\s*(\w+)|(impl)\b)"
)
_MODIFIER = re.compile(r"(?:\b(?:pub|unsafe|async|extern|default|const)|\bpub\s*\([^)]*\))\s*$")
_OPEN, _CLOSE = "([{", ")]}"


def _item_position(masked: str, index: int) -> bool:
    head = masked[:index].rstrip()
    while True:
        m = _MODIFIER.search(head)
        if not m:
            break
        head = head[: m.start()].rstrip()
    return head == "" or head[-1] in "{};]"


def _match_close(masked: str, open_index: int) -> int:
    depth = 0
    for j in range(open_index, len(masked)):
        if masked[j] in _OPEN:
            depth += 1
        elif masked[j] in _CLOSE:
            depth -= 1
            if depth == 0:
                return j
    return len(masked) - 1


def _end_of_item(masked: str, kind: str, begin: int) -> int:
    """Offset of the last character of the item whose name ends at begin."""
    length = len(masked)
    if kind in ("const", "static"):
        depth = 0
        for j in range(begin, length):
            ch = masked[j]
            if ch in _OPEN:
                depth += 1
            elif ch in _CLOSE:
                depth -= 1
            elif ch == ";" and depth <= 0:
                return j
        return length - 1
    if kind == "macro_rules":
        for j in range(begin, length):
            if masked[j] in _OPEN:
                end = _match_close(masked, j)
                rest = masked[end + 1 :]
                stripped = rest.lstrip()
                if stripped.startswith(";"):
                    return end + 1 + (len(rest) - len(stripped))
                return end
        return length - 1
    depth = 0
    for j in range(begin, length):
        ch = masked[j]
        if ch in "([":
            depth += 1
        elif ch in ")]":
            depth -= 1
        elif ch == "{" and depth <= 0:
            return _match_close(masked, j)
        elif ch == ";" and depth <= 0:
            return j
    return length - 1


def _impl_aliases(header: str) -> Tuple[str, frozenset]:
    header = " ".join(header.split())
    rest = re.sub(r"^<[^{}]*?>\s*", "", header) if header.startswith("<") else header
    if " for " in rest:
        rest = rest.split(" for ", 1)[1]
    ident = re.match(r"[&\w:]*?(\w+)\s*(?:<|where|$)", rest)
    aliases = {header}
    if ident:
        aliases.add(ident.group(1))
    return header, frozenset(aliases)


def items_of(source: str) -> List[Item]:
    """Every fn, mod, impl, struct, enum, trait, const, static and macro_rules."""
    masked = mask(source)
    lines = source.split("\n")
    masked_lines = masked.split("\n")
    line_starts = [0]
    for k, ch in enumerate(masked):
        if ch == "\n":
            line_starts.append(k + 1)

    def line_of(offset: int) -> int:
        return bisect.bisect_right(line_starts, offset)

    found: List[Item] = []
    for m in _ITEM.finditer(masked):
        if m.group(5):
            kind, name, name_end = "impl", "", m.end()
        elif m.group(3):
            kind, name, name_end = "macro_rules", m.group(4), m.end()
        else:
            kind, name, name_end = m.group(1), m.group(2), m.end()
        if not _item_position(masked, m.start()):
            continue
        end_offset = _end_of_item(masked, kind, name_end)
        aliases = frozenset({name})
        if kind == "impl":
            brace = masked.find("{", name_end)
            semi = masked.find(";", name_end)
            stop = min(x for x in (brace, semi, len(masked)) if x >= 0)
            name, aliases = _impl_aliases(masked[name_end:stop])
        first = line_of(m.start())
        while first > 1:
            above = first - 1
            original = lines[above - 1].strip()
            code = masked_lines[above - 1].rstrip()
            if original.startswith("//"):
                first = above
            elif code.endswith("]"):
                close = line_starts[above - 1] + len(code) - 1
                depth, j = 0, close
                while j >= 0:
                    if masked[j] == "]":
                        depth += 1
                    elif masked[j] == "[":
                        depth -= 1
                        if depth == 0:
                            break
                    j -= 1
                before = masked[:j].rstrip().rstrip("!").rstrip() if j > 0 else ""
                if j < 0 or not before.endswith("#"):
                    break
                first = line_of(len(before) - 1)
            else:
                break
        found.append(Item(kind, name, aliases, first, line_of(end_offset)))
    return found


# ------------------------------------------------------------------ manifest


class TestEntry(NamedTuple):
    number: int
    package: str
    name: str
    file: str


class HelperEntry(NamedTuple):
    number: int
    file: str
    item: str


def parse_manifest(text: str) -> Tuple[List[TestEntry], List[HelperEntry]]:
    tests: List[TestEntry] = []
    helpers: List[HelperEntry] = []
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.rstrip("\r")
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        fields = line.split("\t")
        if fields[0] == "test" and len(fields) == 4 and all(fields[1:]):
            tests.append(TestEntry(number, fields[1], fields[2], fields[3]))
        elif fields[0] == "helper" and len(fields) == 3 and all(fields[1:]):
            helpers.append(HelperEntry(number, fields[1], fields[2]))
        else:
            raise UsageError(
                f"manifest line {number}: want `test<TAB>package<TAB>name<TAB>file` "
                f"or `helper<TAB>file<TAB>item`, got {line!r}"
            )
    return tests, helpers


def last_segment(test_name: str) -> str:
    return test_name.rsplit("::", 1)[-1]


def module_path(path: str) -> Optional[List[str]]:
    """The module path a test listed from `path` starts with, or None when
    the layout is not one this repository uses (then no deleted test is
    matched to it: the gate fails closed).

    `crates/<pkg>/src/a/b.rs` is `a::b`, `a/mod.rs` is `a`, `lib.rs` and
    `main.rs` are the root; `crates/<pkg>/tests/it/x.rs` is `x` in the `it`
    binary, whose `main.rs` is the root.
    """
    parts = path.split("/")
    if len(parts) < 4 or parts[0] != "crates" or not path.endswith(".rs"):
        return None
    if parts[2] == "src":
        rest = parts[3:]
    elif parts[2] == "tests" and len(parts) >= 5 and parts[3] == "it":
        rest = parts[4:]
    else:
        return None
    rest[-1] = rest[-1][: -len(".rs")]
    if rest[-1] == "mod" or (len(rest) == 1 and rest[0] in ("lib", "main")):
        rest = rest[:-1]
    return rest


def fns_of(items: List["Item"]) -> List[Tuple[List[str], "Item"]]:
    """Each fn item with the names of the inline modules around it,
    outermost first."""
    ordered = sorted(items, key=lambda i: i.start)
    return [
        ([m.name for m in ordered if m.kind == "mod" and m.start <= fn.start and fn.end <= m.end], fn)
        for fn in ordered
        if fn.kind == "fn"
    ]


def names_this_fn(test_name: str, path: str, mods: List[str], fn: str) -> bool:
    """Whether `test_name` (as `cargo test -- --list` prints it) is exactly
    the test fn `fn` that `path` defines inside the inline modules `mods`
    (outermost first): the file's module path, then `mods`, then `fn`.
    Same-named tests in different files or modules are told apart."""
    module = module_path(path)
    return module is not None and test_name.split("::") == module + mods + [fn]


class SourceCache:
    def __init__(self, read: Callable[[str], Optional[str]]):
        self._read = read
        self._items: Dict[str, Optional[List[Item]]] = {}

    def items(self, path: str) -> Optional[List[Item]]:
        if path not in self._items:
            source = self._read(path)
            self._items[path] = None if source is None else items_of(source)
        return self._items[path]


def validate_manifest(
    tests: List[TestEntry],
    helpers: List[HelperEntry],
    base_tests: Set[Tuple[str, str]],
    sources: SourceCache,
) -> None:
    """Every entry must name something that exists on the base revision."""
    for entry in tests:
        where = f"manifest line {entry.number}"
        if (entry.package, entry.name) not in base_tests:
            raise UsageError(f"{where}: test {entry.name} is not listed for package {entry.package} on base")
        items = sources.items(entry.file)
        if items is None:
            raise UsageError(f"{where}: {entry.file} does not exist on base")
        if not any(names_this_fn(entry.name, entry.file, mods, fn.name) for mods, fn in fns_of(items)):
            raise UsageError(f"{where}: {entry.file} defines no test fn named {entry.name} on base")
    if helpers and not tests:
        # A helper's change is checked through the tests that use it: with
        # none listed, no test would run on base and nothing would be checked.
        raise UsageError(
            f"manifest line {helpers[0].number}: a helper entry needs the tests that use it as `test` entries"
        )
    for entry in helpers:
        where = f"manifest line {entry.number}"
        items = sources.items(entry.file)
        if items is None:
            raise UsageError(f"{where}: {entry.file} does not exist on base")
        if not any(entry.item in i.aliases for i in items):
            raise UsageError(f"{where}: no item {entry.item} in {entry.file} on base")


# ------------------------------------------------------------------ the gate


def removed_lines(diff: str) -> List[Tuple[str, int]]:
    """(file, base line) of every line the diff removes or modifies.

    A blank or `//`-comment line is left out: it carries no behaviour and no
    assertion, and the blank line between two tests sits outside both, so
    deleting a test would otherwise always fail the gate (seen on the first
    proof dispatches, relais#306)."""
    out: List[Tuple[str, int]] = []
    current: Optional[str] = None
    old_left = new_left = 0
    header = re.compile(r"@@ -(\d+)(?:,(\d+))? \+\d+(?:,(\d+))? @@")
    next_old = 0
    for line in diff.split("\n"):
        if old_left > 0 or new_left > 0:
            if line.startswith("-"):
                text = line[1:].strip()
                if current and text and not text.startswith("//"):
                    out.append((current, next_old))
                next_old += 1
                old_left -= 1
            elif line.startswith("+"):
                new_left -= 1
            elif line.startswith("\\"):
                pass
            else:
                next_old += 1
                old_left -= 1
                new_left -= 1
            continue
        if line.startswith("--- "):
            path = line[4:].split("\t")[0]
            if path == "/dev/null":
                current = None
            else:
                current = path[2:] if path.startswith("a/") else path
        else:
            m = header.match(line)
            if m:
                next_old = int(m.group(1))
                old_left = 1 if m.group(2) is None else int(m.group(2))
                new_left = 1 if m.group(3) is None else int(m.group(3))
    return out


def lost_coverage(
    removed: List[Tuple[str, int]],
    deleted_names: Set[str],
    tests: List[TestEntry],
    helpers: List[HelperEntry],
    sources: SourceCache,
) -> List[str]:
    """One message per removed base line no affected test or helper covers."""
    # `deleted_names` holds the manifest's tests too (affected = deleted or
    # listed); each is matched to a fn by its exact module path.
    helper_set = {(h.file, h.item) for h in helpers}
    problems: List[str] = []
    for path, line in removed:
        items = sources.items(path)
        enclosing = [i for i in (items or []) if i.start <= line <= i.end]
        mods_of = {fn: mods for mods, fn in fns_of(items or [])}

        def covers(item: Item) -> bool:
            if item in mods_of and any(names_this_fn(n, path, mods_of[item], item.name) for n in deleted_names):
                return True
            return any((path, alias) in helper_set for alias in item.aliases)

        if any(covers(i) for i in enclosing):
            continue
        if enclosing:
            inner = min(enclosing, key=lambda i: i.end - i.start)
            label = f"{inner.kind} {inner.name}".rstrip()
        else:
            label = MODULE_LEVEL
        problems.append(f"{path}:{line}: in {label}")
    return problems


def affected_tests(
    base_tests: Set[Tuple[str, str]], head_tests: Set[Tuple[str, str]], manifest: List[TestEntry]
) -> Set[Tuple[str, str]]:
    return (base_tests - head_tests) | {(t.package, t.name) for t in manifest}


def run_gate(
    diff: str,
    base_tests: Set[Tuple[str, str]],
    head_tests: Set[Tuple[str, str]],
    manifest_text: str,
    read_base: Callable[[str], Optional[str]],
) -> Tuple[int, List[str], Set[Tuple[str, str]]]:
    """(exit code, report lines, affected tests)."""
    sources = SourceCache(read_base)
    try:
        tests, helpers = parse_manifest(manifest_text)
        validate_manifest(tests, helpers, base_tests, sources)
    except UsageError as err:
        return 2, [f"mutants-check: {err}"], set()
    affected = affected_tests(base_tests, head_tests, tests)
    deleted = {n for _, n in base_tests - head_tests}
    removed = [r for r in removed_lines(diff) if r[0].startswith("crates/")]
    problems = lost_coverage(removed, {n for _, n in affected} | deleted, tests, helpers, sources)
    if problems:
        report = ["mutants-check: the manifest does not cover every base line the diff removes or modifies;"]
        report.append("  list the test, or a `helper` entry for the item, in the manifest:")
        report += [f"    {p}" for p in problems]
        return 2, report, affected
    return 0, [], affected


# ----------------------------------------------------------- mutant matching

_NAME = re.compile(r"^(.*?):\d+:\d+: (.*)$")
Key = Tuple[str, str, int]


def index_listed(listed: List[str]) -> Dict[str, Key]:
    """name -> (file, description, nth identical description in that file)."""
    seen: Dict[Tuple[str, str], int] = {}
    index: Dict[str, Key] = {}
    for raw in listed:
        name = raw.strip()
        m = _NAME.match(name)
        if not m:
            continue
        pair = (m.group(1), m.group(2))
        seen[pair] = seen.get(pair, 0) + 1
        index[name] = (pair[0], pair[1], seen[pair])
    return index


def shard_of(names: List[str], shard: str) -> List[str]:
    """Every n-th name starting at k, for `k/n`."""
    try:
        k, n = (int(x) for x in shard.split("/"))
    except ValueError:
        raise UsageError(f"shard {shard!r} is not k/n")
    if n < 1 or not 0 <= k < n:
        raise UsageError(f"shard {shard!r} is not k/n with 0 <= k < n")
    return [name for i, name in enumerate(names) if i % n == k]


# ------------------------------------------------------------ reconciliation


class Outcomes(NamedTuple):
    caught: List[str]
    missed: List[str]
    timeout: List[str]
    unviable: List[str]
    absent: List[str]
    ambiguous: List[str]


def reconcile(requested: List[str], outdir: str) -> Outcomes:
    """Place every requested mutant in exactly one outcome file of mutants.out."""
    sets: Dict[str, Set[str]] = {}
    for outcome in OUTCOME_FILES:
        path = os.path.join(outdir, "mutants.out", outcome + ".txt")
        try:
            with open(path) as handle:
                sets[outcome] = {l.rstrip("\n") for l in handle if l.strip()}
        except FileNotFoundError:
            sets[outcome] = set()
    placed: Dict[str, List[str]] = {k: [] for k in Outcomes._fields}
    for name in requested:
        hits = [o for o in OUTCOME_FILES if name in sets[o]]
        if len(hits) == 1:
            placed[hits[0]].append(name)
        elif not hits:
            placed["absent"].append(name)
        else:
            placed["ambiguous"].append(name)
    return Outcomes(**placed)


def read_lines(path: str) -> List[str]:
    try:
        with open(path) as handle:
            return [l.rstrip("\n") for l in handle if l.strip()]
    except FileNotFoundError:
        return []


def _annotate(title: str, names: List[str]) -> List[str]:
    if os.environ.get("GITHUB_ACTIONS") != "true":
        return []
    return [f"::error title=mutant {title}::{n}" for n in names]


def step1(requested: List[str], outdir: str, listed: List[str]) -> Tuple[int, List[str], List[Key], List[str]]:
    """(exit code, report, at-risk rows as (file, description, nth), their names)."""
    outcomes = reconcile(requested, outdir)
    report: List[str] = []
    incomplete = outcomes.absent + outcomes.ambiguous
    if incomplete:
        report.append(
            f"mutants-check: {len(incomplete)} of {len(requested)} requested mutants have no single outcome on base "
            "(the at-risk set would be incomplete):"
        )
        report += [f"    {n}" for n in incomplete]
        report += _annotate("absent", incomplete)
        return 2, report, [], []
    index = index_listed(listed)
    # A mutant that makes the affected tests hang is one they detect: it is
    # at risk like a caught one, and the remaining suite must catch it too.
    detected = outcomes.caught + outcomes.timeout
    rows = [index[n] for n in detected if n in index]
    names = [n for n in detected if n in index]
    return 0, report, rows, names


class Plan(NamedTuple):
    targets: List[str]  # names on the branch
    gone: List[str]  # base names with no counterpart on the branch
    equivalents: List[Tuple[str, str]]  # (base name, reason)


def parse_equivalents(text: str) -> Dict[str, str]:
    out: Dict[str, str] = {}
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.rstrip("\r")
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        name, tab, reason = line.partition("\t")
        if not tab or not name.strip() or not reason.strip():
            raise UsageError(f"equivalents line {number}: want `mutant name<TAB>reason`, got {line!r}")
        out[name.strip()] = reason.strip()
    return out


def plan_step2(
    at_risk: List[Tuple[Key, str]], head_listed: List[str], equivalents: Dict[str, str]
) -> Plan:
    """Map each at-risk (key, base name) to the branch's name for the same mutant."""
    here = {key: name for name, key in index_listed(head_listed).items()}
    targets: List[str] = []
    gone: List[str] = []
    listed_equivalents: List[Tuple[str, str]] = []
    for key, base_name in at_risk:
        if base_name in equivalents:
            listed_equivalents.append((base_name, equivalents[base_name]))
        elif key in here:
            targets.append(here[key])
        else:
            gone.append(base_name)
    return Plan(targets, gone, listed_equivalents)


def step2_verdict(plan: Plan, outdir: str) -> Tuple[int, List[str]]:
    outcomes = reconcile(plan.targets, outdir)
    report: List[str] = []
    total = len(plan.targets) + len(plan.gone) + len(plan.equivalents)

    def section(heading: str, names: List[str], title: str) -> None:
        if names:
            report.append(f"mutants-check: {len(names)} {heading}:")
            report.extend(f"    {n}" for n in names)
            report.extend(_annotate(title, names))

    section("missed here, caught on base by a deleted test", outcomes.missed, "missed")
    section("no longer a mutant here (the product code changed), caught on base", plan.gone, "gone")
    section("timed out here (incomplete evidence)", outcomes.timeout, "timeout")
    section("unviable here (incomplete evidence)", outcomes.unviable, "unviable")
    section("absent from mutants.out (incomplete evidence)", outcomes.absent, "absent")
    section("in more than one outcome file (incomplete evidence)", outcomes.ambiguous, "ambiguous")
    if plan.equivalents:
        report.append(f"mutants-check: {len(plan.equivalents)} listed as equivalent, not counted:")
        report.extend(f"    {n}\t{why}" for n, why in plan.equivalents)
    report.append(f"    {len(outcomes.caught)} of {len(plan.targets)} still caught ({total} at risk)")
    incomplete = outcomes.timeout + outcomes.unviable + outcomes.absent + outcomes.ambiguous
    if incomplete:
        code = 2
    elif outcomes.missed or plan.gone:
        code = 1
    else:
        code = 0
    if code == 0:
        report.append("mutants-check: nothing lost")
    return code, report


# ----------------------------------------------------------------------- CLI


def _read_text(path: str) -> str:
    if not path:
        return ""
    try:
        with open(path) as handle:
            return handle.read()
    except OSError as err:
        raise UsageError(f"cannot read {path}: {err}")


def _tests_from(path: str) -> Set[Tuple[str, str]]:
    out = set()
    for line in read_lines(path):
        package, _, name = line.partition("\t")
        out.add((package, name))
    return out


def _write(path: str, lines: List[str]) -> None:
    with open(path, "w") as handle:
        handle.writelines(l + "\n" for l in lines)


def _emit(lines: List[str], stream=sys.stdout) -> None:
    for line in lines:
        print(line, file=stream)


def main(argv: List[str]) -> int:
    parser = argparse.ArgumentParser(prog="mutants_check.py")
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("check-equivalents")
    p.add_argument("file")
    p = sub.add_parser("gate")
    for flag in ("diff", "base_root", "base_tests", "head_tests", "manifest", "affected_out"):
        p.add_argument("--" + flag.replace("_", "-"), dest=flag, default="")
    p = sub.add_parser("shard")
    p.add_argument("--in", dest="source", required=True)
    p.add_argument("--shard", default="")
    p.add_argument("--out", required=True)
    p = sub.add_parser("step1")
    for flag in ("requested", "outdir", "listed", "at_risk_out"):
        p.add_argument("--" + flag.replace("_", "-"), dest=flag, required=True)
    p = sub.add_parser("plan")
    for flag in ("at_risk", "head_listed", "equivalents", "targets_out", "gone_out", "equivalents_out"):
        p.add_argument("--" + flag.replace("_", "-"), dest=flag, default="")
    p = sub.add_parser("verdict")
    for flag in ("targets", "gone", "equivalents_out", "outdir"):
        p.add_argument("--" + flag.replace("_", "-"), dest=flag, required=True)
    args = parser.parse_args(argv)
    try:
        return _dispatch(args)
    except UsageError as err:
        print(f"mutants-check: {err}", file=sys.stderr)
        return 2


def _dispatch(args) -> int:
    if args.command == "check-equivalents":
        parse_equivalents(_read_text(args.file))
        return 0
    if args.command == "gate":
        def read_base(path: str) -> Optional[str]:
            try:
                with open(os.path.join(args.base_root, path)) as handle:
                    return handle.read()
            except (OSError, UnicodeDecodeError):
                return None

        code, report, affected = run_gate(
            _read_text(args.diff),
            _tests_from(args.base_tests),
            _tests_from(args.head_tests),
            _read_text(args.manifest),
            read_base,
        )
        _emit(report, sys.stderr)
        if code == 0:
            _write(args.affected_out, sorted(f"{p}\t{n}" for p, n in affected))
        return code
    if args.command == "shard":
        names = read_lines(args.source)
        _write(args.out, shard_of(names, args.shard) if args.shard else names)
        return 0
    if args.command == "step1":
        code, report, rows, names = step1(read_lines(args.requested), args.outdir, read_lines(args.listed))
        _emit(report, sys.stderr)
        if code == 0:
            _write(args.at_risk_out, [f"{f}\t{d}\t{i}\t{n}" for (f, d, i), n in zip(rows, names)])
        return code
    if args.command == "plan":
        at_risk: List[Tuple[Key, str]] = []
        for line in read_lines(args.at_risk):
            f, d, i, n = line.split("\t")
            at_risk.append(((f, d, int(i)), n))
        plan = plan_step2(at_risk, read_lines(args.head_listed), parse_equivalents(_read_text(args.equivalents)))
        _write(args.targets_out, plan.targets)
        _write(args.gone_out, plan.gone)
        _write(args.equivalents_out, [f"{n}\t{why}" for n, why in plan.equivalents])
        return 0
    if args.command == "verdict":
        equivalents = [tuple(l.split("\t", 1)) for l in read_lines(args.equivalents_out)]
        plan = Plan(read_lines(args.targets), read_lines(args.gone), equivalents)
        code, report = step2_verdict(plan, args.outdir)
        _emit(report)
        return code
    raise UsageError(f"unknown command {args.command}")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
