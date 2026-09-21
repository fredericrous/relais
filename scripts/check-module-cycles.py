#!/usr/bin/env python3
"""Architecture gates over `crates/relais/src`, run by `make lint` and CI.

Two properties that a compiler cannot state and a reviewer cannot keep in
their head:

1. The module graph is acyclic (fleet constraint
   `components.acyclic-dependencies`). Rust happily compiles a cycle
   between two modules of one crate; nothing else notices that `policy`
   and `route` each reach into the other until a reader tries to
   understand either one alone.
2. `policy` is pure (fleet constraint `architecture.dependency-rule`):
   it decides authority from parsed documents and never touches the
   filesystem, the process table or the environment. PATH probes live in
   `tooling`, repository files in `repo`.

The graph is built from `use crate::X` and `crate::X::` in non-test code:
test modules may reach anywhere, since a cycle through a test is not a
cycle in the shipped design. Python 3 rather than shell because both the
macOS workstation and the ubuntu CI runner have it, and neither has a
portable way to do this in bash 3.2.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent / "crates" / "relais" / "src"

# `main.rs` is the binary crate and `lib.rs` the library root: both are
# allowed to name every module, and neither is a module anything imports.
ROOTS = {"main.rs", "lib.rs"}

USE_CRATE = re.compile(r"\buse\s+crate::(?:\{)?([a-z_0-9:,\s{}]+)")
PATH_CRATE = re.compile(r"\bcrate::([a-z_][a-z_0-9]*)")

IMPURE = {
    "std::fs": "the filesystem",
    "std::process": "the process table",
    "std::env": "the environment",
}
# Modules whose non-test code must stay free of the operations above.
PURE_MODULES = {"policy"}


def strip_comments_and_strings(line: str) -> str:
    """Blank out `//` comments and string literals, so a `crate::x` in
    prose or a brace in a JSON fixture never reaches the parser."""
    out = []
    i = 0
    n = len(line)
    while i < n:
        c = line[i]
        if c == "/" and i + 1 < n and line[i + 1] == "/":
            break
        if c == '"':
            i += 1
            while i < n:
                if line[i] == "\\":
                    i += 2
                    continue
                if line[i] == '"':
                    i += 1
                    break
                i += 1
            out.append(" ")
            continue
        out.append(c)
        i += 1
    return "".join(out)


def production_lines(path: Path) -> list[str]:
    """The file's non-test lines, comments and string literals removed.

    A top-level `#[cfg(test)]` opens the in-file test module, which every
    file in this crate puts last; everything from there on is test code.
    """
    lines = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        code = strip_comments_and_strings(raw)
        if code.startswith("#[cfg(test)]"):
            break
        lines.append(code)
    return lines


def module_of(path: Path) -> str:
    return path.relative_to(SRC).parts[0].removesuffix(".rs")


def imports(lines: list[str]) -> set[str]:
    found: set[str] = set()
    for line in lines:
        for group in USE_CRATE.findall(line):
            for item in re.split(r"[,{}\s]+", group):
                head = item.split("::")[0].strip()
                if head:
                    found.add(head)
        found.update(PATH_CRATE.findall(line))
    return found


def build_graph() -> dict[str, dict[str, set[str]]]:
    """module -> imported module -> the files that import it."""
    graph: dict[str, dict[str, set[str]]] = {}
    for path in sorted(SRC.rglob("*.rs")):
        if path.name in ROOTS and path.parent == SRC:
            continue
        module = module_of(path)
        edges = graph.setdefault(module, {})
        for target in imports(production_lines(path)):
            if target == module:
                continue
            edges.setdefault(target, set()).add(str(path.relative_to(SRC)))
    return graph


def strongly_connected(graph: dict[str, dict[str, set[str]]]) -> list[list[str]]:
    """Every group of two or more modules that can all reach each other —
    Tarjan's algorithm, iterative so a deep graph cannot exhaust the
    recursion limit. Each group is one cycle to break, reported whole
    rather than as a single arbitrary loop through it."""
    index: dict[str, int] = {}
    low: dict[str, int] = {}
    on_stack: set[str] = set()
    stack: list[str] = []
    groups: list[list[str]] = []
    counter = 0

    for root in sorted(graph):
        if root in index:
            continue
        work: list[tuple[str, list[str]]] = [
            (root, [child for child in sorted(graph[root]) if child in graph])
        ]
        index[root] = low[root] = counter
        counter += 1
        stack.append(root)
        on_stack.add(root)
        while work:
            node, children = work[-1]
            if children:
                child = children.pop()
                if child not in index:
                    index[child] = low[child] = counter
                    counter += 1
                    stack.append(child)
                    on_stack.add(child)
                    work.append(
                        (child, [c for c in sorted(graph[child]) if c in graph])
                    )
                elif child in on_stack:
                    low[node] = min(low[node], index[child])
                continue
            work.pop()
            if work:
                low[work[-1][0]] = min(low[work[-1][0]], low[node])
            if low[node] == index[node]:
                group = []
                while True:
                    member = stack.pop()
                    on_stack.discard(member)
                    group.append(member)
                    if member == node:
                        break
                if len(group) > 1:
                    groups.append(sorted(group))
    return groups


def cycle_edges(
    graph: dict[str, dict[str, set[str]]], group: list[str]
) -> list[str]:
    members = set(group)
    lines = []
    for source in group:
        for target in sorted(graph[source]):
            if target in members:
                where = ", ".join(sorted(graph[source][target]))
                lines.append(f"    {source} -> {target} from {where}")
    return lines


def purity_failures() -> list[str]:
    failures = []
    for path in sorted(SRC.rglob("*.rs")):
        module = module_of(path)
        if module not in PURE_MODULES or path.name in ROOTS:
            continue
        for number, line in enumerate(production_lines(path), start=1):
            for needle, what in IMPURE.items():
                if needle in line:
                    failures.append(
                        f"{path.relative_to(SRC)}:{number}: `{module}` reaches "
                        f"{what} (`{needle}`); that belongs in an adapter module"
                    )
    return failures


def main() -> int:
    graph = build_graph()
    problems = 0

    for group in strongly_connected(graph):
        problems += 1
        print("module cycle: {" + ", ".join(group) + "}", file=sys.stderr)
        for line in cycle_edges(graph, group):
            print(line, file=sys.stderr)

    for failure in purity_failures():
        problems += 1
        print(failure, file=sys.stderr)

    if problems:
        print(
            f"{problems} architecture problem(s); see "
            "scripts/check-module-cycles.py for what is checked",
            file=sys.stderr,
        )
        return 1
    print(f"module graph: {len(graph)} modules, no cycles; policy is pure")
    return 0


if __name__ == "__main__":
    sys.exit(main())
