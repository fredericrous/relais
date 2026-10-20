"""Does a commit only move code? For the module split (plan
docs/plans/2026-10-19-finish-admission-then-split.md, Phase 4).

Compares the multiset of code tokens a commit removes with the multiset it
adds. Before tokenising, it drops `use` and `mod` statements (multi-line
ones whole), `#[cfg(test)]` lines and `mod x {` openers (with the `}` that
closes each), strips visibility (`pub`, `pub(crate)`, `pub(super)`,
`pub(in ...)`) and `crate::`, `super::`, `self::` and `relais::` path
prefixes. Tokens make rustfmt's re-wrapping invisible. Two additions are
structure and accepted: splitting an impl block (`impl`, the type's name,
`{`, `}`) and rustfmt's trailing comma on a wrapped list. Comments are not
judged: the comment lines a commit adds are printed for a person to read.

Usage: python3 scripts/check-move-only.py <repo> <commit>
Exit 0 when the code only moved, 1 otherwise."""
import re, subprocess, sys
from collections import Counter
repo, commit = sys.argv[1], sys.argv[2]
diff = subprocess.run(["git","-C",repo,"show","--format=","-U0","--no-renames",commit],
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
MOD_BLOCK = re.compile(r'^\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*\{\s*$')
mod_blocks = Counter()
new_comment_lines, old_comment_lines = Counter(), Counter()
for l in diff.splitlines():
    if l.startswith(('+++','---')): continue
    side = l[:1]
    if side in in_use:
        body = l[1:]
        if CFG_TEST.match(body):
            continue
        if MOD_BLOCK.match(body):
            mod_blocks[side] += 1
            continue
        # A `use` statement spans lines until its `;`: skip all of it.
        if in_use[side] or SKIP.match(body):
            in_use[side] = not body.rstrip().endswith(';')
            continue
        if COMMENT.match(body):
            (new_comment_lines if side == '+' else old_comment_lines)[body.strip()] += 1
    if l.startswith('+'):
        m = re.match(r'\s*impl(?:<[^>]*>)?\s+(\w+)', l[1:])
        if m: impl_types.add(m.group(1))
        (doc_add if COMMENT.match(l[1:]) else add).update(tokens(l[1:]))
    elif l.startswith('-'):
        (doc_rem if COMMENT.match(l[1:]) else rem).update(tokens(l[1:]))
only_rem, only_add = rem - add, add - rem
structural = {'impl', '{', '}', ','} | impl_types  # ',' : rustfmt's trailing comma on a wrapped list
# A module block opened on one side closes with a `}` there too.
only_add['}'] -= mod_blocks['+'] - mod_blocks['-']
only_add = +only_add
struct_add = Counter({k: v for k, v in only_add.items() if k in structural})
real_add = only_add - struct_add
print(f"tokens: removed-only {sum(only_rem.values())}, added-only {sum(real_add.values())}, structure {dict(struct_add)}")
for k, v in only_rem.most_common(20): print(f"- {v}x {k}")
for k, v in real_add.most_common(20): print(f"+ {v}x {k}")
# impl headers come in whole: as many `impl` as `{` and `}` each
ok = not only_rem and not real_add and struct_add.get('{',0) == struct_add.get('}',0) == struct_add.get('impl',0)
new_doc = sum((doc_add - doc_rem).values()); lost_doc = sum((doc_rem - doc_add).values())
print(f"comments: added-only tokens {new_doc}, removed-only tokens {lost_doc}")
for line in (new_comment_lines - old_comment_lines).elements(): print(f"  new comment: {line[:150]}")
print("MOVE-ONLY (code): yes" if ok else "MOVE-ONLY (code): NO")
sys.exit(0 if ok else 1)
