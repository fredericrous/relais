#!/usr/bin/env python3
"""Tests for the CI platform scripts (stdlib only, deterministic):
ci-platform-touched.sh over a real throwaway history, ci-platform-plan.sh
over its environment, ci-release-platforms.sh over a check-runs fixture."""

import json
import os
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


def script(name):
    return os.path.join(HERE, name)


def git(repo, *args):
    env = dict(os.environ)
    for key in [k for k in env if k.startswith("GIT_")]:
        del env[key]
    env.update(
        GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t", GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@t", GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
    )
    return subprocess.run(
        ["git", "-c", "core.hooksPath=/dev/null", *args], cwd=repo, env=env,
        check=True, capture_output=True, text=True,
    ).stdout.strip()


def write(repo, path, text):
    full = os.path.join(repo, path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w") as f:
        f.write(text)


class Touched(unittest.TestCase):
    """The last line says whether the change is platform-specific."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = self.tmp.name
        git(self.repo, "init", "-q", "-b", "main")
        write(self.repo, "README.md", "hello\n")
        write(self.repo, "crates/relais/src/ipc.rs", "#[cfg(unix)]\nfn a() {}\n")
        write(self.repo, "crates/relais/src/plain.rs", "fn b() {}\n")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "base")
        self.base = git(self.repo, "rev-parse", "HEAD")

    def tearDown(self):
        self.tmp.cleanup()

    def change(self, path, text):
        write(self.repo, path, text)
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "change")

    def verdict(self, base=None):
        out = subprocess.run(
            ["sh", script("ci-platform-touched.sh"), base or self.base],
            cwd=self.repo, check=True, capture_output=True, text=True,
        ).stdout.strip().splitlines()
        return out[-1], out[:-1]

    def test_documentation_only_is_not_platform_code(self):
        self.change("README.md", "hello again\n")
        self.assertEqual(self.verdict()[0], "false")

    def test_a_portable_source_file_is_not_platform_code(self):
        self.change("crates/relais/src/plain.rs", "fn b() { let _ = 1; }\n")
        self.assertEqual(self.verdict()[0], "false")

    def test_a_cfg_gated_source_file_is_platform_code(self):
        self.change("crates/relais/src/ipc.rs", "#[cfg(unix)]\nfn a() { }\n")
        verdict, reasons = self.verdict()
        self.assertEqual(verdict, "true")
        self.assertIn("crates/relais/src/ipc.rs: compiles per platform", reasons)

    def test_a_cfg_gated_test_is_not_platform_code(self):
        self.change("crates/relais/tests/it/x.rs", "#[cfg(unix)]\n#[test]\nfn t() {}\n")
        self.assertEqual(self.verdict()[0], "false")

    def test_build_inputs_are_platform_code(self):
        for path in [".github/workflows/ci.yaml", "scripts/x.sh", "Makefile",
                     "Cargo.lock", "crates/relais/Cargo.toml", "install/install.sh"]:
            with self.subTest(path=path):
                self.change(path, path + "\n")
                self.assertEqual(self.verdict()[0], "true")
                git(self.repo, "reset", "-q", "--hard", self.base)

    def test_an_unreadable_base_runs_every_platform(self):
        self.assertEqual(self.verdict("0" * 40)[0], "true")


class Plan(unittest.TestCase):
    """Which legs run, and the summary line that names the deferred ones."""

    def plan(self, **env):
        with tempfile.TemporaryDirectory() as d:
            out = os.path.join(d, "out")
            summary = os.path.join(d, "summary")
            full = {k: v for k, v in os.environ.items()
                    if k not in ("EVENT", "PLATFORM_CODE", "FULL_CI")}
            full.update(env, GITHUB_OUTPUT=out, GITHUB_STEP_SUMMARY=summary)
            subprocess.run(["sh", script("ci-platform-plan.sh")], env=full,
                           check=True, capture_output=True, text=True)
            with open(out) as f:
                values = dict(line.split("=", 1) for line in f.read().splitlines())
            with open(summary) as f:
                return values, f.read()

    def oses(self, values):
        return [leg["os"] for leg in json.loads(values["test_matrix"])]

    def test_a_portable_pull_request_tests_macos_and_says_what_it_deferred(self):
        values, summary = self.plan(EVENT="pull_request", PLATFORM_CODE="false", FULL_CI="false")
        self.assertEqual(values["run_other"], "false")
        self.assertEqual(self.oses(values), ["macos-latest"])
        self.assertIn("test (ubuntu-latest), test (windows-latest): deferred to the push to main", summary)

    def test_platform_code_or_the_label_runs_every_platform(self):
        for env in [dict(PLATFORM_CODE="true", FULL_CI="false"),
                    dict(PLATFORM_CODE="false", FULL_CI="true"),
                    dict()]:
            with self.subTest(env=env):
                values, summary = self.plan(EVENT="pull_request", **env)
                self.assertEqual(values["run_other"], "true")
                self.assertEqual(self.oses(values), ["macos-latest", "ubuntu-latest", "windows-latest"])
                self.assertNotIn("deferred", summary)

    def test_main_and_the_schedule_test_every_platform(self):
        for event in ["push", "schedule"]:
            with self.subTest(event=event):
                values, _ = self.plan(EVENT=event, PLATFORM_CODE="false", FULL_CI="false")
                self.assertEqual(values["run_other"], "true")
                self.assertEqual(len(self.oses(values)), 3)

    def test_each_leg_names_the_platform_its_attestation_must_match(self):
        values, _ = self.plan(EVENT="push")
        platforms = {leg["os"]: leg["platform"] for leg in json.loads(values["test_matrix"])}
        self.assertEqual(platforms, {"macos-latest": "aarch64-macos",
                                     "ubuntu-latest": "x86_64-linux",
                                     "windows-latest": "x86_64-windows"})


class ReleasePlatforms(unittest.TestCase):
    """The release waits for all three `test` legs on the tagged commit."""

    def run_with(self, runs):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump({"check_runs": runs}, f)
            path = f.name
        try:
            env = dict(os.environ, CHECK_RUNS_JSON=path)
            return subprocess.run(["sh", script("ci-release-platforms.sh"), "abc123"],
                                  env=env, capture_output=True, text=True)
        finally:
            os.unlink(path)

    def leg(self, os_name, conclusion="success"):
        return {"name": f"test ({os_name})", "conclusion": conclusion}

    def test_every_leg_passed_releases(self):
        guard = self.run_with([self.leg("macos-latest"), self.leg("ubuntu-latest"),
                                self.leg("windows-latest"), self.leg("lint", "success")])
        self.assertEqual(guard.returncode, 0, guard.stderr)

    def test_a_missing_or_failed_leg_blocks_and_is_named(self):
        guard = self.run_with([self.leg("macos-latest"), self.leg("ubuntu-latest"),
                                self.leg("windows-latest", "failure")])
        self.assertEqual(guard.returncode, 1)
        self.assertIn("test (windows-latest)", guard.stderr)
        guard = self.run_with([self.leg("macos-latest")])
        self.assertEqual(guard.returncode, 1)
        self.assertIn("test (ubuntu-latest)", guard.stderr)

    def test_an_unreadable_answer_blocks(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            f.write("not json")
            path = f.name
        try:
            env = dict(os.environ, CHECK_RUNS_JSON=path)
            guard = subprocess.run(["sh", script("ci-release-platforms.sh"), "abc123"],
                                    env=env, capture_output=True, text=True)
        finally:
            os.unlink(path)
        self.assertEqual(guard.returncode, 1)


if __name__ == "__main__":
    unittest.main()
