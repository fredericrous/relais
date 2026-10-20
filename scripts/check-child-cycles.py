"""The dependency graph between the child modules of one module directory
(crates/relais/src/ledger, crates/relais/src/runner), and its cycles.

scripts/check-module-cycles.py stops at the top-level module, so a cycle
between two children of `ledger` is invisible to it. This reads each
child's production code (before its `#[cfg(test)]`), follows `super::x::`
paths and resolves names imported through the parent's re-exports to the
child that defines them. Written for the module split (plan
docs/plans/2026-10-19-finish-admission-then-split.md, Phase 4).

Usage: python3 scripts/check-child-cycles.py <dir>
Prints each child's dependencies; exit 1 when there is a cycle."""
import re, pathlib, sys
root = pathlib.Path(sys.argv[1])
mods = {}
for p in root.rglob('*.rs'):
    name = p.parent.name if p.stem == 'mod' else p.stem
    if p.parent == root and p.stem == 'mod': name = '__facade__'
    mods[name] = p
DEF = re.compile(r'^\s*pub(?:\([^)]*\))?\s+(?:async\s+)?(?:struct|enum|fn|const|type|trait|static)\s+(\w+)', re.M)
defined = {}
for m, p in mods.items():
    if m == '__facade__': continue
    for n in DEF.findall(p.read_text()): defined.setdefault(n, m)
def prod(text, path=None):
    """The production part: everything before the file's test module. A
    top-level `#[cfg(test)]` that is not a single trailing `mod` would hide
    code after it, so that layout is refused, as check-module-cycles.py
    refuses it."""
    marks = [m.start() for m in re.finditer(r'^#\[cfg\(test\)\]\s*$', text, re.M)]
    if not marks:
        return text
    rest = text[marks[0]:].split('\n', 1)[1] if len(marks) == 1 else ''
    if len(marks) > 1 or not re.match(r'(pub(\([^)]*\))?\s+)?mod\s+\w+\s*(;|\{)', rest):
        sys.exit(f"{path}: a top-level #[cfg(test)] that is not one trailing test module; cannot tell production from tests")
    return text[:marks[0]]
edges = {}
for m, p in mods.items():
    if m in ('__facade__', 'test_support'): continue
    t = prod(p.read_text(), p)
    deps = set(re.findall(r'\bsuper::(\w+)::', t))
    for group in re.findall(r'use super::\{([^}]*)\}', t, re.S):
        for n in re.findall(r'\b(\w+)\b', group):
            if n in defined: deps.add(defined[n])
            elif n in mods: deps.add(n)
    for n in re.findall(r'use super::(\w+);', t):
        deps.add(defined.get(n, n))
    for n in re.findall(r'\bsuper::(\w+)\b(?!::)', t):
        if n in defined: deps.add(defined[n])
    edges[m] = sorted((deps & set(mods)) - {m, '__facade__'})
for m in sorted(edges): print(f"{m}: {', '.join(edges[m]) or '-'}")
found = []
def dfs(n, stack, seen):
    if n in stack: found.append(stack[stack.index(n):] + [n]); return
    if n in seen: return
    seen.add(n); stack.append(n)
    for d in edges.get(n, []): dfs(d, stack, seen)
    stack.pop()
seen = set()
for n in edges: dfs(n, [], seen)
print('CYCLES:', found if found else 'none')
sys.exit(1 if found else 0)
