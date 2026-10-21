"""The dependency graph between the child modules of one module directory
(crates/relais/src/ledger, crates/relais/src/runner), and its cycles.

scripts/check-module-cycles.py stops at the top-level module, so a cycle
between two children of `ledger` is invisible to it. This reads each
child's production code (before its trailing `#[cfg(test)]` module) and
resolves what it names to the child that defines it:

- a module is keyed by its path below the directory (`engine`,
  `review::engine`), never by its file name alone;
- `super::…` and `crate::<dir>::…` paths resolve to the module they name;
- a name imported through a parent's re-exports resolves to the child that
  defines it;
- `tests/` and `test_support` are test code and are not read;
- a glob import of a parent or sibling (`use super::*;`) is refused: it
  hides exactly the dependencies this check exists to see.

Written for the module split (plan
docs/plans/2026-10-19-finish-admission-then-split.md, Phase 4).

Usage: python3 scripts/check-child-cycles.py <dir>
Prints each child's dependencies; exit 1 when there is a cycle, exit 2 when
a file cannot be read this way."""
import re, pathlib, sys

root = pathlib.Path(sys.argv[1])
root_name = root.name


def key_of(path):
    rel = path.relative_to(root).with_suffix('')
    parts = list(rel.parts)
    if parts[-1] == 'mod':
        parts = parts[:-1]
    return '::'.join(parts)  # '' is the directory's own mod.rs, the facade


mods = {}
for p in sorted(root.rglob('*.rs')):
    rel = p.relative_to(root).parts
    if 'tests' in rel[:-1] or p.stem in ('tests', 'test_support'):
        continue
    mods[key_of(p)] = p

DEF = re.compile(r'^\s*pub(?:\([^)]*\))?\s+(?:async\s+)?(?:struct|enum|fn|const|type|trait|static)\s+(\w+)', re.M)


def prod(text, path):
    """Everything before the file's test module. A top-level `#[cfg(test)]`
    that is not a single trailing `mod` would hide code after it, so that
    layout is refused, as check-module-cycles.py refuses it."""
    marks = [m.start() for m in re.finditer(r'^#\[cfg\(test\)\]\s*$', text, re.M)]
    if not marks:
        return text
    rest = text[marks[0]:].split('\n', 1)[1] if len(marks) == 1 else ''
    if len(marks) > 1 or not re.match(r'(pub(\([^)]*\))?\s+)?mod\s+\w+\s*(;|\{)', rest):
        sys.exit(f"{path}: a top-level #[cfg(test)] that is not one trailing test module; cannot tell production from tests")
    return text[:marks[0]]


sources = {k: prod(p.read_text(), p) for k, p in mods.items()}
defined = {}
for k, t in sources.items():
    if k == '':
        continue
    for n in DEF.findall(t):
        defined.setdefault(n, k)


def parent(key):
    return key.rsplit('::', 1)[0] if '::' in key else ''


def resolve(key, path):
    """The module key a `super::…`/`crate::<dir>::…`/`self::…` path names,
    and the item it ends in (or None when it names a module)."""
    segs = path.split('::')
    if segs[0] == 'crate':
        if len(segs) < 2 or segs[1] != root_name:
            return None, None
        base, segs = '', segs[2:]
    else:
        base = key
        while segs and segs[0] in ('super', 'self'):
            base = parent(base) if segs[0] == 'super' else base
            segs = segs[1:]
    cur = base
    for i, s in enumerate(segs):
        nxt = f"{cur}::{s}" if cur else s
        if nxt in mods:
            cur = nxt
        else:
            return cur, s
    return cur, None


USE = re.compile(r'\buse\s+((?:crate|super|self)(?:::\w+)*)::(\{[^;]*\}|\w+|\*)\s*;', re.S)
PATH = re.compile(r'\b((?:crate::\w+|super|self)(?:::\w+)+)')
edges, globs = {}, []
for key, text in sources.items():
    if key == '':
        continue
    deps = set()
    for prefix, tail in USE.findall(text):
        if tail == '*':
            target, _ = resolve(key, prefix)
            if target is not None and (prefix.startswith('super') or prefix.startswith(f'crate::{root_name}')):
                globs.append(f"{mods[key]}: `use {prefix}::*;`")
            continue
        names = re.findall(r'\b(\w+)\b', tail.strip('{}'))
        for n in names:
            if n in ('self',):
                continue
            target, item = resolve(key, f"{prefix}::{n}")
            if target is None:
                continue
            owner = target
            if item is not None and target in ('',) + tuple(parent(key) for _ in [0]):
                owner = defined.get(item, target)
            deps.add(owner)
    for path in PATH.findall(re.sub(r'\buse\s+[^;]*;', '', text)):
        target, item = resolve(key, path)
        if target is None:
            continue
        deps.add(defined.get(item, target) if item is not None and target == '' else target)
    edges[key] = sorted(d for d in deps if d and d != key and not key.startswith(d + '::'))

if globs:
    print("glob imports of a parent or sibling (name what is used):")
    for g in globs:
        print(f"  {g}")
    sys.exit(2)

for m in sorted(edges):
    print(f"{m}: {', '.join(edges[m]) or '-'}")
found = []


def dfs(n, stack, seen):
    if n in stack:
        found.append(stack[stack.index(n):] + [n])
        return
    if n in seen:
        return
    seen.add(n)
    stack.append(n)
    for d in edges.get(n, []):
        dfs(d, stack, seen)
    stack.pop()


seen = set()
for n in sorted(edges):
    dfs(n, [], seen)
print('CYCLES:', [' -> '.join(c) for c in found] if found else 'none')
sys.exit(1 if found else 0)
