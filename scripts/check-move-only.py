"""Does a commit only move code? For the module split (plan
docs/plans/2026-10-19-finish-admission-then-split.md, Phase 4).

Compares the multiset of code tokens a commit removes with the multiset it
adds. Before tokenising, it drops `use` and `mod` statements (multi-line
ones whole), `#[cfg(test)]` lines and `mod x {` openers (with the `}` that
closes each), strips visibility (`pub`, `pub(crate)`, `pub(super)`,
`pub(in ...)`) and `crate::`, `super::`, `self::` and `relais::` path
prefixes. Tokens make rustfmt's re-wrapping invisible. Two additions are
structure and accepted: splitting an impl block (`impl`, the type's name,
`{`, `}`) and commas that end a line (rustfmt wrapping a list), up to the
number of comma-ended lines the commit adds. Item-level `#[cfg(test)]`
attributes must balance; one on a `mod x {` header is a moved test module.
Every item whose visibility a move changed is printed for a person to read. Comments are not
judged: the comment lines a commit adds are printed for a person to read.

Usage: python3 scripts/check-move-only.py <repo> <commit>
Exit 0 when the code only moved, 1 otherwise."""
import re, subprocess, sys
from collections import Counter
repo, commit = sys.argv[1], sys.argv[2]
diff = subprocess.run(["git","-C",repo,"show","--format=","-U8","--no-renames",commit],
                      capture_output=True, text=True, check=True).stdout
SKIP = re.compile(r'^\s*(pub(\([^)]*\))?\s+)?(use|mod)\s')
VIS = re.compile(r'\bpub(\((crate|super|self|in [^)]*)\))?\s*')
QUAL = re.compile(r'\b(?:(?:crate|super|self|relais)::)+(?:[a-z_][a-z0-9_]*::)*')
TOK = re.compile(r'[A-Za-z_][A-Za-z0-9_]*|\d+|"(?:[^"\\]|\\.)*"|\S')
def tokens(text):
    if SKIP.match(text) or not text.strip(): return []
    return TOK.findall(QUAL.sub('', VIS.sub('', text)))
rem, add = Counter(), Counter()
doc_rem, doc_add = Counter(), Counter()
impl_types = set()
COMMENT = re.compile(r'^\s*//')
in_use = {'+': False, '-': False}
CFG_TEST = re.compile(r'^\s*#\[cfg\(test\)\]\s*$')
# `#![cfg(test)]` at the top of a file makes the file a test module.
CFG_TEST_FILE = re.compile(r'^\s*#!\[cfg\(test\)\]\s*$')
MOD_BLOCK = re.compile(r'^\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*\{\s*$')
mod_blocks = Counter()
cfg_tests = Counter()
cfg_counted = {'+': False, '-': False}  # the attribute above was a changed line on that side
trailing_commas = Counter()
VIS_ITEM = re.compile(r'^\s*(pub(?:\([^)]*\))?\s+)?(?:(?:async|const|unsafe|extern "C")\s+)*(fn|struct|enum|trait|type|const|static|mod)\s+(\w+)')
# (enclosing impl type, kind, name) -> visibilities seen, counted per side:
# two same-named items (two `fn new`) cannot overwrite each other.
visibility = {'+': Counter(), '-': Counter()}
IMPL_OPEN = re.compile(r'^impl(?:<[^>]*>)?\s+(?:[\w:<>, ]+\s+for\s+)?(\w+)')
current_impl = {'+': '', '-': ''}
prev = {'+': '', '-': ''}
new_comment_lines, old_comment_lines = Counter(), Counter()
for l in diff.splitlines():
    if l.startswith(('+++','---','@@','diff ','index ')): 
        if l.startswith('@@'): in_use = {'+': False, '-': False}
        continue
    side = l[:1]
    if side == ' ':
        # Context: no tokens counted, but a `use` statement that starts or
        # continues here decides whether the changed lines inside it are
        # imports.
        body = l[1:]
        for sd in ('+', '-'):
            if in_use[sd] or SKIP.match(body):
                in_use[sd] = not body.rstrip().endswith(';')
            prev[sd] = body
            cfg_counted[sd] = False
        continue
    if side in in_use:
        body = l[1:]
        if not CFG_TEST.match(body) and not MOD_BLOCK.match(body) and not re.match(r'^\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*;', body):
            cfg_counted[side] = False
        if CFG_TEST_FILE.match(body):
            prev[side] = body
            continue
        if CFG_TEST.match(body):
            cfg_tests[side] += 1
            cfg_counted[side] = True
            prev[side] = body
            continue
        if MOD_BLOCK.match(body):
            mod_blocks[side] += 1
            # `#[cfg(test)] mod tests {`: a test module's header, not an
            # attribute on a moved item.
            if CFG_TEST.match(prev[side]) and cfg_counted[side]:
                cfg_tests[side] -= 1
            prev[side] = body
            continue
        prev[side] = body
        if re.match(r'^\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*;', body) and CFG_TEST.match(prev[side]) and cfg_counted[side]:
            cfg_tests[side] -= 1
        # A `use` statement spans lines until its `;`: skip all of it.
        if in_use[side] or SKIP.match(body):
            in_use[side] = not body.rstrip().endswith(';')
            continue
        o = IMPL_OPEN.match(body)
        if o:
            current_impl[side] = o.group(1)
        elif body.startswith('}'):
            current_impl[side] = ''
        m = VIS_ITEM.match(body)
        if m:
            # An indented item whose `impl` header lies outside the diff's
            # context has an unknown owner ('?'), matched by kind and name.
            owner = (current_impl[side] or '?') if body.startswith((' ', '\t')) else ''
            key = (owner, m.group(2), m.group(3))
            visibility[side][key + ((m.group(1) or 'private').strip(),)] += 1
        # rustfmt's wrapping puts one item per line, each ending in `,`.
        # Counted per side: only that many added commas are free, and none
        # in the middle of a line.
        if body.rstrip().endswith(','):
            trailing_commas[side] += 1
        if COMMENT.match(body):
            (new_comment_lines if side == '+' else old_comment_lines)[body.strip()] += 1
    # An impl header (`impl<'a> RunEngine<'a> {`) is structure as a whole:
    # its generics are no more code than the `impl` keyword itself.
    IMPL_HEADER = re.match(r'\s*impl\b[^;{]*\{\s*$', l[1:]) if l[:1] in '+-' else None
    if IMPL_HEADER:
        m = re.match(r'\s*impl(?:<[^>]*>)?\s+(\w+)', l[1:])
        if m and l.startswith('+'): impl_types.add(m.group(1))
        (add if l.startswith('+') else rem).update(['impl', m.group(1) if m else 'impl', '{'])
        continue
    if l.startswith('+'):
        m = re.match(r'\s*impl(?:<[^>]*>)?\s+(\w+)', l[1:])
        if m: impl_types.add(m.group(1))
        (doc_add if COMMENT.match(l[1:]) else add).update(tokens(l[1:]))
    elif l.startswith('-'):
        (doc_rem if COMMENT.match(l[1:]) else rem).update(tokens(l[1:]))
only_rem, only_add = rem - add, add - rem
structural = {'impl', '{', '}', ','} | impl_types
# A module block opened on one side closes with a `}` there too.
only_add['}'] -= max(0, mod_blocks['+'] - mod_blocks['-'])
only_rem['}'] -= max(0, mod_blocks['-'] - mod_blocks['+'])
# rustfmt joining a wrapped list drops its end-of-line commas.
only_rem[','] -= max(0, trailing_commas['-'] - trailing_commas['+'])
only_add, only_rem = +only_add, +only_rem
struct_add = Counter({k: v for k, v in only_add.items() if k in structural})
real_add = only_add - struct_add
print(f"tokens: removed-only {sum(only_rem.values())}, added-only {sum(real_add.values())}, structure {dict(struct_add)}")
for k, v in only_rem.most_common(20): print(f"- {v}x {k}")
for k, v in real_add.most_common(20): print(f"+ {v}x {k}")
# impl headers come in whole: as many `impl` as `{` and `}` each
free_commas = max(0, trailing_commas['+'] - trailing_commas['-'])
commas_ok = struct_add.get(',', 0) <= free_commas
cfg_ok = cfg_tests['+'] == cfg_tests['-']
ok = (not only_rem and not real_add and commas_ok and cfg_ok
      and struct_add.get('{',0) == struct_add.get('}',0) == struct_add.get('impl',0))
if not commas_ok: print(f"commas: {struct_add.get(',',0)} added, only {free_commas} close a wrapped list")
if not cfg_ok: print(f"#[cfg(test)]: {cfg_tests['-']} removed, {cfg_tests['+']} added")
# Visibility is stripped before the comparison; report every item whose
# visibility a move changed, so a person reads each widening.
changes = Counter()
gained, lost = visibility['+'] - visibility['-'], visibility['-'] - visibility['+']
for (owner, kind, name, after), n in sorted(gained.items()):
    befores = [v for (o, k, nm, v), c in lost.items()
               if (k, nm) == (kind, name) and (o == owner or '?' in (o, owner)) for _ in range(c)]
    before = befores[0] if befores else None
    where = f"{owner}::" if owner else ""
    if before is not None and before != after:
        changes[(before, after)] += n
        match = next(key for key in lost if key[1:] == (kind, name, before) and (key[0] == owner or '?' in (key[0], owner)))
        lost[match] -= 1
        lost = +lost
    if after == 'pub' and before != 'pub':
        print(f"  visibility: {kind} {where}{name}: {before or 'new item'} -> pub")
if changes:
    print("visibility: " + ", ".join(f"{n} {b} -> {a}" for (b, a), n in sorted(changes.items())))
new_doc = sum((doc_add - doc_rem).values()); lost_doc = sum((doc_rem - doc_add).values())
print(f"comments: added-only tokens {new_doc}, removed-only tokens {lost_doc}")
for line in (new_comment_lines - old_comment_lines).elements(): print(f"  new comment: {line[:150]}")
print("MOVE-ONLY (code): yes" if ok else "MOVE-ONLY (code): NO")
sys.exit(0 if ok else 1)
