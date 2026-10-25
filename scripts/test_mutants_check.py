#!/usr/bin/env python3
"""Fixture tests for scripts/mutants_check.py (stdlib only, deterministic)."""

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import mutants_check as mc

BASE_SOURCE = """\
use std::fmt;

const LIMIT: usize = 3;

fn product(x: u32) -> u32 {
    x + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn helper() -> u32 {
        7
    }

    #[test]
    fn keeps() {
        assert_eq!(product(1), 2);
        assert_eq!(helper(), 7);
    }

    #[test]
    fn goes() {
        assert_eq!(product(2), 3);
    }
}
"""
FILE = "crates/relais/src/lib.rs"
KEPT_ASSERTION = "        assert_eq!(product(1), 2);"


def line_of(text: str) -> int:
    return BASE_SOURCE.split("\n").index(text) + 1


def diff_removing(*numbers: int, path: str = FILE) -> str:
    out = [f"--- a/{path}", f"+++ b/{path}"]
    for number in numbers:
        out += [f"@@ -{number} +{number - 1},0 @@", "-x"]
    return "\n".join(out) + "\n"


def gate(diff, manifest="", base=None, head=None):
    base = base if base is not None else {("relais", "tests::keeps"), ("relais", "tests::goes")}
    head = head if head is not None else {("relais", "tests::keeps")}
    return mc.run_gate(diff, base, head, manifest, lambda p: BASE_SOURCE if p == FILE else None)


class Scan(unittest.TestCase):
    def test_spans_include_attributes_and_nest(self):
        items = {(i.kind, i.name): i for i in mc.items_of(BASE_SOURCE)}
        goes = items[("fn", "goes")]
        lines = BASE_SOURCE.split("\n")
        self.assertEqual(lines[goes.start - 1].strip(), "#[test]")
        self.assertEqual(lines[goes.end - 1], "    }")
        self.assertEqual(items[("mod", "tests")].start, line_of("#[cfg(test)]"))
        self.assertIn(("const", "LIMIT"), items)

    def test_braces_in_strings_comments_and_chars_do_not_break_the_scan(self):
        source = (
            "fn tricky() {\n"
            '    let a = "}}}";\n'
            "    let b = '}';\n"
            "    let c = '\\'';\n"
            '    let d = r#"} " {"#;\n'
            "    // } }\n"
            "    /* { /* } */ { */\n"
            "    let e: &'static str = \"{\";\n"
            "}\n"
            "fn after() {}\n"
        )
        items = {i.name: i for i in mc.items_of(source)}
        self.assertEqual((items["tricky"].start, items["tricky"].end), (1, 9))
        self.assertEqual((items["after"].start, items["after"].end), (10, 10))

    def test_item_kinds(self):
        source = (
            "macro_rules! m { () => { 1 }; }\n"
            "pub(crate) struct S { a: u8 }\n"
            "struct T(u8);\n"
            "enum E { A, B }\n"
            'static NAME: &str = "x;";\n'
            "impl<T> fmt::Display for Foo<T> where T: X {\n"
            "    fn fmt(&self) {}\n"
            "}\n"
            "const fn k() -> u8 { 1 }\n"
            "fn takes(f: impl Fn() -> u8) -> [u8; 2] { [0; 2] }\n"
        )
        items = {(i.kind, i.name): i for i in mc.items_of(source)}
        self.assertEqual((items[("macro_rules", "m")].start, items[("macro_rules", "m")].end), (1, 1))
        self.assertEqual(items[("struct", "S")].end, 2)
        self.assertEqual(items[("struct", "T")].end, 3)
        self.assertEqual(items[("enum", "E")].end, 4)
        self.assertEqual(items[("static", "NAME")].end, 5)
        impls = [i for i in items.values() if i.kind == "impl"]
        self.assertEqual(len(impls), 1)
        self.assertIn("Foo", impls[0].aliases)
        self.assertEqual((impls[0].start, impls[0].end), (6, 8))
        self.assertEqual(items[("fn", "k")].end, 9)
        self.assertEqual(items[("fn", "takes")].end, 10)


class Gate(unittest.TestCase):
    def test_removed_assertion_in_unlisted_retained_test_fails(self):
        code, report, _ = gate(diff_removing(line_of(KEPT_ASSERTION)))
        self.assertEqual(code, 2)
        self.assertIn(f"{FILE}:{line_of(KEPT_ASSERTION)}: in fn keeps", "\n".join(report))

    def test_same_test_listed_passes(self):
        manifest = f"# why\ntest\trelais\ttests::keeps\t{FILE}\n"
        code, _, affected = gate(diff_removing(line_of(KEPT_ASSERTION)), manifest)
        self.assertEqual(code, 0)
        self.assertIn(("relais", "tests::keeps"), affected)

    def test_removed_lines_of_a_disappeared_test_pass(self):
        code, _, _ = gate(diff_removing(line_of("        assert_eq!(product(2), 3);")))
        self.assertEqual(code, 0)

    def test_a_same_named_test_deleted_elsewhere_does_not_cover(self):
        # `keeps` is deleted from another file; this file's `keeps` stays,
        # and the assertion removed from it is not covered by that deletion.
        base = {("relais", "tests::keeps"), ("relais", "other::tests::keeps")}
        head = {("relais", "tests::keeps")}
        code, report, _ = gate(diff_removing(line_of(KEPT_ASSERTION)), base=base, head=head)
        self.assertEqual(code, 2)
        self.assertIn("in fn keeps", "\n".join(report))

    def test_module_paths_follow_the_layout(self):
        cases = [
            ("crates/relais/src/lib.rs", []),
            ("crates/relais/src/main.rs", []),
            ("crates/relais/src/route.rs", ["route"]),
            ("crates/relais/src/runner/mod.rs", ["runner"]),
            ("crates/relais/src/runner/tests/verify_kind.rs", ["runner", "tests", "verify_kind"]),
            ("crates/relais/tests/it/main.rs", []),
            ("crates/relais/tests/it/release_scenarios.rs", ["release_scenarios"]),
            ("crates/relais/tests/it/common/mod.rs", ["common"]),
            ("crates/relais/tests/other.rs", None),
            ("crates/relais/build.rs", None),
        ]
        for path, want in cases:
            with self.subTest(path=path):
                self.assertEqual(mc.module_path(path), want)
        scan = "a_world_is_countable_by_doctors_scan"
        release = "crates/relais/tests/it/release_scenarios.rs"
        self.assertTrue(mc.names_this_fn(f"release_scenarios::{scan}", release, [], scan))
        self.assertFalse(mc.names_this_fn(f"hook_respond::{scan}", release, [], scan))
        self.assertTrue(mc.names_this_fn("route::tests::x", "crates/relais/src/route.rs", ["tests"], "x"))
        self.assertFalse(mc.names_this_fn("route::x", "crates/relais/src/route.rs", ["tests"], "x"))

    def test_removed_use_or_const_outside_helpers_fails(self):
        code, report, _ = gate(diff_removing(line_of("use std::fmt;")))
        self.assertEqual(code, 2)
        self.assertIn(mc.MODULE_LEVEL, "\n".join(report))
        code, report, _ = gate(diff_removing(line_of("const LIMIT: usize = 3;")))
        self.assertEqual(code, 2)
        self.assertIn("const LIMIT", "\n".join(report))

    def test_helper_entry_covers_its_item(self):
        manifest = f"helper\t{FILE}\tLIMIT\ntest\trelais\ttests::keeps\t{FILE}\n"
        code, _, affected = gate(diff_removing(line_of("const LIMIT: usize = 3;")), manifest)
        self.assertEqual(code, 0)
        self.assertIn(("relais", "tests::keeps"), affected)

    def test_a_helper_entry_without_any_test_entry_is_refused(self):
        # Nothing would run on base: the change to LIMIT would go unchecked.
        code, report, _ = gate(diff_removing(line_of("const LIMIT: usize = 3;")), f"helper\t{FILE}\tLIMIT\n")
        self.assertEqual(code, 2)
        self.assertIn("needs the tests that use it", "\n".join(report))

    def test_a_test_entry_must_name_its_fn_by_module_path(self):
        # `keeps` exists in FILE, but as tests::keeps, not other::keeps.
        base = {("relais", "tests::keeps"), ("relais", "tests::goes"), ("relais", "other::keeps")}
        code, report, _ = gate(diff_removing(line_of(KEPT_ASSERTION)), f"test\trelais\tother::keeps\t{FILE}\n", base=base)
        self.assertEqual(code, 2)
        self.assertIn("defines no test fn named other::keeps", "\n".join(report))

    def test_addition_only_diff_with_empty_manifest_passes(self):
        diff = f"--- a/{FILE}\n+++ b/{FILE}\n@@ -3,0 +4,2 @@\n+fn new() {{}}\n+\n"
        both = {("relais", "tests::keeps")}
        code, report, affected = gate(diff, "", base=both, head=both)
        self.assertEqual((code, report), (0, []))
        self.assertEqual(affected, set())

    def test_minus_lines_inside_hunks_are_not_headers(self):
        diff = f"--- a/{FILE}\n+++ b/{FILE}\n@@ -1,2 +1,1 @@\n--- not a header\n-b\n+c\n"
        self.assertEqual(mc.removed_lines(diff), [(FILE, 1), (FILE, 2)])

    def test_invalid_manifest_entries_exit_2(self):
        for manifest in (
            f"test\trelais\ttests::missing\t{FILE}\n",
            "test\trelais\ttests::keeps\tcrates/none.rs\n",
            f"test\trelais\ttests::keeps\t{FILE}x\n",
            f"helper\t{FILE}\tNOPE\n",
            "helper\tonly-two\n",
            f"test\trelais\ttests::product\t{FILE}\n",
        ):
            code, report, _ = gate("", manifest)
            self.assertEqual(code, 2, manifest)
            self.assertTrue(report and "manifest" in report[0], manifest)

    def test_equivalents_without_reason_are_a_usage_error(self):
        with self.assertRaises(mc.UsageError):
            mc.parse_equivalents("a.rs:1:1: replace f with ()\n")
        self.assertEqual(mc.parse_equivalents("# c\nm\twhy\n"), {"m": "why"})


class Reconcile(unittest.TestCase):
    def outdir(self, **files):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        if files:
            os.makedirs(os.path.join(tmp.name, "mutants.out"))
        for name, lines in files.items():
            with open(os.path.join(tmp.name, "mutants.out", name + ".txt"), "w") as handle:
                handle.write("".join(l + "\n" for l in lines))
        return tmp.name

    def verdict(self, targets, outdir, gone=(), equivalents=()):
        return mc.step2_verdict(mc.Plan(list(targets), list(gone), list(equivalents)), outdir)

    def test_all_caught_is_0(self):
        self.assertEqual(self.verdict(["a", "b"], self.outdir(caught=["a", "b"]))[0], 0)

    def test_missed_is_1(self):
        self.assertEqual(self.verdict(["a", "b"], self.outdir(caught=["a"], missed=["b"]))[0], 1)

    def test_gone_is_1(self):
        self.assertEqual(self.verdict(["a"], self.outdir(caught=["a"]), gone=["g"])[0], 1)

    def test_timeout_is_2(self):
        self.assertEqual(self.verdict(["a", "b"], self.outdir(caught=["a"], timeout=["b"]))[0], 2)

    def test_a_mutant_in_two_outcome_files_is_2(self):
        code, report = self.verdict(["a"], self.outdir(caught=["a"], missed=["a"]))
        self.assertEqual(code, 2)
        self.assertIn("    a", report)

    def test_step1_timeout_is_not_dropped(self):
        # A hang on base is a detection: the mutant is at risk.
        listed = ["f.rs:1:1: d", "f.rs:2:1: e"]
        code, _, rows, names = mc.step1(listed, self.outdir(caught=["f.rs:1:1: d"], timeout=["f.rs:2:1: e"]), listed)
        self.assertEqual(code, 0)
        self.assertEqual(sorted(names), sorted(listed))
        self.assertEqual(len(rows), 2)

    def test_caught_plus_unviable_is_2(self):
        self.assertEqual(self.verdict(["a", "b"], self.outdir(caught=["a"], unviable=["b"]))[0], 2)

    def test_absent_from_truncated_outcomes_is_2(self):
        code, report = self.verdict(["a", "b"], self.outdir(caught=["a"]))
        self.assertEqual(code, 2)
        self.assertIn("    b", report)

    def test_missing_outcome_files_is_2(self):
        self.assertEqual(self.verdict(["a"], self.outdir())[0], 2)

    def test_2_wins_over_1(self):
        self.assertEqual(self.verdict(["a", "b"], self.outdir(missed=["a"], timeout=["b"]))[0], 2)

    def test_equivalent_missed_mutant_is_0_and_listed(self):
        plan = mc.plan_step2(
            [(("f.rs", "d", 1), "f.rs:1:1: d"), (("f.rs", "e", 1), "f.rs:2:1: e")],
            ["f.rs:5:1: d", "f.rs:6:1: e"],
            {"f.rs:2:1: e": "same behaviour"},
        )
        self.assertEqual(plan.targets, ["f.rs:5:1: d"])
        code, report = mc.step2_verdict(plan, self.outdir(caught=["f.rs:5:1: d"], missed=["f.rs:6:1: e"]))
        self.assertEqual(code, 0)
        self.assertIn("    f.rs:2:1: e\tsame behaviour", report)

    def test_step1_absent_is_2(self):
        listed = ["f.rs:1:1: d", "f.rs:2:1: e"]
        code, _, _, _ = mc.step1(listed, self.outdir(caught=["f.rs:1:1: d"]), listed)
        self.assertEqual(code, 2)

    def test_step1_collects_caught_as_at_risk(self):
        listed = ["f.rs:1:1: d", "f.rs:2:1: d", "f.rs:3:1: e"]
        outdir = self.outdir(caught=["f.rs:2:1: d"], missed=["f.rs:1:1: d", "f.rs:3:1: e"])
        code, _, rows, names = mc.step1(listed, outdir, listed)
        self.assertEqual((code, rows, names), (0, [("f.rs", "d", 2)], ["f.rs:2:1: d"]))

    def test_keys_survive_moved_lines_and_count_occurrences(self):
        base = ["f.rs:1:1: d", "f.rs:9:1: d"]
        head = ["f.rs:4:1: d", "f.rs:20:1: d"]
        plan = mc.plan_step2([(("f.rs", "d", 2), base[1])], head, {})
        self.assertEqual(plan.targets, ["f.rs:20:1: d"])

    def test_shard_is_round_robin(self):
        self.assertEqual(mc.shard_of(list("abcde"), "1/2"), ["b", "d"])


if __name__ == "__main__":
    unittest.main()
