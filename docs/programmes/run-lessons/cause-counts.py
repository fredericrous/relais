#!/usr/bin/env python3
"""Weekly relais run failures by cause, each over the runs it could fire on.

    cause-counts.py [--weeks N] [--include-current] [--class CLASS]
                    [--json] [--no-header] [-q]
                    [--fingerprint FILE [--rebaseline] [--helper PATH]]
    cause-counts.py --sample CAUSE [-n N] [--since DAYS] [--json]
    cause-counts.py --list-causes [--class CLASS]
    cause-counts.py --self-test

Reads the relais ledger read-only (`--ledger`, default
~/.local/state/relais/ledger.sqlite) and each run's `events.jsonl` under the
artifacts directory (`--artifacts`, default `runs/` beside the ledger). A run
counts in the UTC ISO week (labelled by its Monday) of `runs.created_at`, on
both sides of its rate. Replay trials (`/worktrees/replay/` in the repository
path) are left out. The window is the last N complete weeks; the current week
only with --include-current, marked `partial`.

Each cause has a class: `guidance` (what a hook, skill or rule can address),
`infra` (the machine or the harness), `unclassified` (a known reason whose
detail matches no rule: `<reason>:unclassified`) or `unlisted` (a failure
reason the table does not map: `<reason>:unlisted`). `--list-causes` is the
one source of the cause names; retro's `relais:` causes copy it.

Output: TSV with one header line (`--no-header` drops it):
`week partial cause class runs denominator_name denominator rate`; `-` as the
rate of a zero denominator. `--json` prints the same rows as JSON. Warnings go
to stderr: unclassified rows, mapped reasons with no row in the window, runs
with no events file (`review unknown`), runs with review findings but no
reviewer dispatch. `-q` hides the info lines only (the unfingerprinted note
and the review check when it found nothing).

`--fingerprint FILE` records the recipe (this script's blob, the sources, the
cause table, the window rule; never dates) through greenbelay's
`tools/measure_fingerprint.py`, loaded by path from `--helper` or
`$MEASURE_FINGERPRINT`, and refuses a changed recipe before printing a row.

Exit codes: 0 done (also when stdout closes early, as by `| head`); 1 ledger,
artifacts or fingerprint file unreadable, or the helper fails to load; 2 usage;
3 fingerprint refused; 4 a --self-test assertion failed.
"""

import argparse
import contextlib
import datetime as dt
import importlib.util
import json
import os
import pathlib
import re
import sqlite3
import sys
import tempfile

STATE = pathlib.Path(os.path.expanduser("~/.local/state/relais"))
REPLAY = "/worktrees/replay/"
CUT = 400

# cause, class, denominator. The order is the output order.
CAUSES = [
    ("scope_exceeded", "guidance", "dispatched"),
    ("check_failed", "guidance", "verified"),
    ("criteria_unmet", "guidance", "verified"),
    ("criterion_unreported", "guidance", "verified"),
    ("same_failure_recurrence", "guidance", "verified"),
    ("review_findings", "guidance", "reviewed"),
    ("gate_absent", "infra", "verified"),
    ("setup_incomplete", "infra", "verified"),
    ("blocked_preflight", "infra", "created"),
    ("admission_wait_exhausted", "infra", "created"),
    ("disk_low", "infra", "created"),
    ("process_crash", "infra", "created"),
    ("limit_reached", "infra", "created"),
    ("env_missing", "infra", "created"),
    ("runner_failure", "infra", "created"),
    ("review_unavailable", "infra", "created"),
]
CLASS = {c: k for c, k, _ in CAUSES}
DENOM = {c: d for c, _, d in CAUSES}
# Reasons counted under their own name.
SAME_NAME = {"scope_exceeded", "criteria_unmet", "same_failure_recurrence"} | {
    c for c, k, _ in CAUSES if k == "infra" and c not in ("gate_absent", "setup_incomplete")
}
SPLIT = {"behavioral_failure": "verified", "verification_gap": "verified"}
REVIEW = {"review_findings", "review_repair"}
# Every ledger reason the table maps; one with no row in the window is warned.
MAPPED = sorted(SAME_NAME | set(SPLIT) | REVIEW)
# Reasons that are steps or answers, not failures: never `:unlisted`.
NOT_FAILURE = {
    "worker_dispatched", "verification_started", "verification_inputs_changed",
    "checks_and_review_passed", "worktree_retired", "plan_accepted",
    "package_started", "package_finished", "decision_approved",
    "decision_rejected", "decision_revised", "decision_recorded",
    "decision_salvaged", "decision_abandoned", "cancelled_by_user",
    "cancelled_without_worker", "reconciled_interrupted", "write_lease_wait",
    "write_lease_held", "model_substitution_approved",
}
GAP_RULES = [
    ("acceptance criterion", "criterion_unreported"),
    ("amont gate", "gate_absent"),
    ("verification setup", "setup_incomplete"),
]
DENOM_REASON = {"dispatched": "worker_dispatched", "verified": "verification_started"}
REVIEWER = '"agent_kind":"reviewer"'
WINDOW_RULE = "last {n} complete ISO weeks (UTC, Monday), include_current={c}"
REVIEW_RULE = ("reviewed = a dispatch_started with agent_kind reviewer in "
               "<artifacts>/<run>/events.jsonl")


class Unreadable(Exception):
    pass


def classify(reason, detail_json):
    """The causes one transition counts toward (several for a gap list)."""
    if reason in SAME_NAME:
        return [reason]
    if reason in REVIEW:
        return ["review_findings"]
    if reason in SPLIT:
        try:
            detail = json.loads(detail_json or "{}")
        except ValueError:
            detail = {}
        if not isinstance(detail, dict):
            detail = {}
        if reason == "behavioral_failure":
            if detail.get("failures"):
                return ["check_failed"]
            if detail.get("criteria"):
                return ["criteria_unmet"]
            return ["behavioral_failure:unclassified"]
        out = []
        gaps = detail.get("gaps")
        for gap in gaps if isinstance(gaps, list) and gaps else [None]:
            text = gap if isinstance(gap, str) else ""
            hit = next((c for p, c in GAP_RULES if text.startswith(p)), None)
            out.append(hit or "verification_gap:unclassified")
        return out
    if reason in NOT_FAILURE:
        return []
    return [f"{reason}:unlisted"]


def class_of(cause):
    if cause in CLASS:
        return CLASS[cause]
    return cause.rsplit(":", 1)[1]


def denom_of(cause):
    if cause in DENOM:
        return DENOM[cause]
    reason = cause.split(":", 1)[0]
    return SPLIT.get(reason, "created")


def monday(when):
    day = when.astimezone(dt.timezone.utc).date()
    return day - dt.timedelta(days=day.weekday())


FRACTION = re.compile(r"(\.\d{6})\d+")


def parse_ts(text):
    """An aware UTC datetime, or None for a missing, malformed or naive value.
    relais writes nine fractional digits; older Pythons read at most six."""
    if not isinstance(text, str):
        return None
    try:
        when = dt.datetime.fromisoformat(FRACTION.sub(r"\1", text.replace("Z", "+00:00")))
    except ValueError:
        return None
    return when if when.tzinfo else None


def window(today, weeks, include_current):
    current = today - dt.timedelta(days=today.weekday())
    mondays = [current - dt.timedelta(weeks=k) for k in range(weeks, 0, -1)]
    if include_current:
        mondays.append(current)
    return mondays, current


def open_ledger(path):
    if not path.is_file():
        raise Unreadable(f"no ledger at {path}; try --ledger PATH")
    try:
        con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        con.execute("select 1 from runs limit 1")
        return con
    except sqlite3.Error as err:
        raise Unreadable(f"cannot read the ledger {path} ({err}); try --ledger PATH") from err


def reviewed(artifacts, run):
    """True, False, or None when the run has no events file."""
    path = artifacts / run / "events.jsonl"
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            return any(REVIEWER in line and "dispatch_started" in line for line in fh)
    except FileNotFoundError:
        return None
    except OSError as err:
        raise Unreadable(f"cannot read {path} ({err}); try --artifacts PATH") from err


def count(con, artifacts, today, weeks, include_current):
    """Rows and the stderr warnings, nothing printed."""
    mondays, current = window(today, weeks, include_current)
    keep = set(mondays)
    runs = {}
    bad_time = []
    con.execute("begin")  # one read snapshot for both queries
    for rid, repo, created in con.execute("select id, repo_path, created_at from runs"):
        if REPLAY in (repo or ""):
            continue
        when = parse_ts(created)
        if when is None:
            bad_time.append(rid)
            continue
        week = monday(when)
        if week in keep:
            runs[rid] = week
    causes = {r: set() for r in runs}
    seen = {r: set() for r in runs}
    reasons = set()
    for rid, reason, detail in con.execute(
            "select run_id, reason, detail_json from transitions order by id"):
        if rid not in runs:
            continue
        seen[rid].add(reason)
        reasons.add(reason)
        causes[rid].update(classify(reason, detail))
    con.execute("commit")
    sides = {}
    unknown = []
    for rid, week in runs.items():
        side = {"created"}
        for name, reason in DENOM_REASON.items():
            if reason in seen[rid]:
                side.add(name)
        if "verified" in side:
            got = reviewed(artifacts, rid)
            if got is None:
                unknown.append(rid)
            elif got:
                side.add("reviewed")
        sides[rid] = side
    extra = sorted({c for s in causes.values() for c in s if c not in CLASS})
    order = [c for c, _, _ in CAUSES] + extra
    rows = []
    for week in mondays:
        in_week = [r for r, w in runs.items() if w == week]
        for cause in order:
            dname = denom_of(cause)
            den = sum(1 for r in in_week if dname in sides[r])
            num = sum(1 for r in in_week if cause in causes[r] and dname in sides[r])
            rows.append({
                "week": week.isoformat(),
                "partial": "yes" if week == current else "no",
                "cause": cause,
                "class": class_of(cause),
                "runs": num,
                "denominator_name": dname,
                "denominator": den,
                "rate": round(num / den, 4) if den else None,
            })
    review_runs = [r for r in runs if seen[r] & REVIEW]
    lost = set(unknown)
    unreviewed = [r for r in review_runs if "reviewed" not in sides[r] and r not in lost]
    outside = {}
    for rid, found in causes.items():
        for cause in found:
            if denom_of(cause) not in sides[rid]:
                outside.setdefault(cause, []).append(rid)
    unclassified = sum(r["runs"] for r in rows if r["class"] == "unclassified")
    notes = {
        "zero": [m for m in MAPPED if m not in reasons],
        "unclassified": unclassified,
        "unclassified_names": [c for c in extra if c.endswith(":unclassified")],
        "unknown": unknown,
        "review_runs": len(review_runs),
        "unreviewed": unreviewed,
        "outside": outside,
        "bad_time": bad_time,
        "runs": len(runs),
    }
    return rows, notes


def recipe(helper, ledger, artifacts, weeks, include_current):
    return helper.recipe(
        pathlib.Path(__file__).resolve(),
        f"python3.{sys.version_info.minor}",
        f"relais ledger {ledger}; artifacts {artifacts}",
        "transitions(run_id, reason, detail_json); runs(id, repo_path, created_at); "
        "events.jsonl dispatch_started.agent_kind",
        {
            "causes": [list(c) for c in CAUSES],
            "gap_rules": [list(g) for g in GAP_RULES],
            "not_failure": sorted(NOT_FAILURE),
            "exclude": REPLAY,
            "review": REVIEW_RULE,
            "week_of": "runs.created_at, both sides of a rate",
        },
        WINDOW_RULE.format(n=weeks, c=include_current),
    )


def load_helper(path):
    spec = importlib.util.spec_from_file_location("measure_fingerprint", path)
    if spec is None or spec.loader is None:
        raise ImportError(f"not a Python module: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


NEED_HELPER = ("cause-counts: --fingerprint needs the greenbelay helper: give "
               "`--helper PATH` or `MEASURE_FINGERPRINT=PATH` "
               "(greenbelay's tools/measure_fingerprint.py)")


def fingerprint(args, coverage):
    """None when recorded, else the exit code."""
    where = args.helper or os.environ.get("MEASURE_FINGERPRINT")
    try:
        helper = load_helper(pathlib.Path(where).expanduser())
        wanted = recipe(helper, args.ledger, args.artifacts, args.weeks,
                        args.include_current)
    except Exception as err:  # any failure to load the helper or build its recipe
        print(f"cause-counts: cannot load the fingerprint helper {where} ({err})",
              file=sys.stderr)
        return 1
    record = helper.rebaseline if args.rebaseline else helper.check
    try:
        outcome = record(args.fingerprint, wanted, coverage)
    except helper.Refused as refused:
        print(refused.render(helper.accept_command()), file=sys.stderr)
        return 3
    except helper.SidecarError as err:
        print(f"cause-counts: {err}", file=sys.stderr)
        return 1
    if not args.quiet:
        print(f"fingerprint: {type(outcome).__name__.lower()} in {args.fingerprint}",
              file=sys.stderr)
    return None


def warn(notes, quiet):
    if notes["bad_time"]:
        print(f"warning: {len(notes['bad_time'])} runs have a missing, malformed or "
              "naive created_at and are left out: " + " ".join(notes["bad_time"][:5]),
              file=sys.stderr)
    if notes["unclassified"]:
        names = ", ".join(notes["unclassified_names"])
        print(f"warning: {notes['unclassified']} unclassified rows; see "
              f"--sample {notes['unclassified_names'][0]}"
              + (f" ({names})" if len(notes["unclassified_names"]) > 1 else ""),
              file=sys.stderr)
    if notes["zero"]:
        print("warning: mapped reasons with no row in the window: "
              + ", ".join(notes["zero"]), file=sys.stderr)
    if notes["unknown"]:
        print(f"warning: {len(notes['unknown'])} verified runs have no events file "
              f"(review unknown, left out of `reviewed`): "
              + " ".join(notes["unknown"][:5]), file=sys.stderr)
    for cause, rids in sorted(notes["outside"].items()):
        print(f"warning: {len(rids)} `{cause}` runs never reached `{denom_of(cause)}` "
              f"and are left out of its rate: " + " ".join(rids[:5]), file=sys.stderr)
    bad = notes["unreviewed"]
    if bad:
        print(f"warning: {len(bad)} of {notes['review_runs']} runs with review "
              f"findings have no reviewer dispatch: " + " ".join(bad[:5]),
              file=sys.stderr)
    elif not quiet:
        print(f"info: all {notes['review_runs']} runs with review findings have a "
              "reviewer dispatch", file=sys.stderr)


COLUMNS = ["week", "partial", "cause", "class", "runs", "denominator_name",
           "denominator", "rate"]


def emit(rows, args):
    if args.json:
        json.dump(rows, sys.stdout, indent=1)
        sys.stdout.write("\n")
        return
    if not args.no_header:
        print("\t".join(COLUMNS))
    for r in rows:
        rate = "-" if r["rate"] is None else f"{r['rate']:.4f}"
        print("\t".join(str(r[c]) for c in COLUMNS[:-1]) + "\t" + rate)


def repo_name(path):
    return re.sub(r"-wt-.*$", "", os.path.basename((path or "").rstrip("/")))


def valid_sample(cause):
    if cause in CLASS:
        return True
    reason, _, tail = cause.partition(":")
    return (tail == "unclassified" and reason in SPLIT) or (
        tail == "unlisted" and bool(reason))


def sample(con, args, now):
    since = (now - dt.timedelta(days=args.since)).isoformat()
    out = []
    sampled = set()
    for rid, repo, at, reason, detail in con.execute(
            "select t.run_id, r.repo_path, t.at, t.reason, t.detail_json "
            "from transitions t join runs r on r.id = t.run_id "
            "where t.at >= ? order by t.at desc", (since,)):
        if REPLAY in (repo or "") or args.sample not in classify(reason, detail):
            continue
        if rid in sampled:  # one row per run, its latest
            continue
        sampled.add(rid)
        out.append({"run": rid, "repo": repo_name(repo), "at": at,
                    "reason": reason, "detail": detail or ""})
        if len(out) >= args.n:
            break
    if args.json:
        json.dump(out, sys.stdout, indent=1)
        sys.stdout.write("\n")
        return
    for s in out:
        text = s["detail"].replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")
        if len(text) > CUT:
            text = text[:CUT] + f"…(+{len(text) - CUT} chars)"
        print("\t".join([s["run"], s["repo"], s["at"], text]))
    if not out:
        print(f"no `{args.sample}` row in the last {args.since} days", file=sys.stderr)


def parser():
    ap = argparse.ArgumentParser(
        prog="cause-counts.py", description=__doc__, allow_abbrev=False,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ledger", type=pathlib.Path, default=STATE / "ledger.sqlite")
    ap.add_argument("--artifacts", type=pathlib.Path,
                    help="run directories (default: runs/ beside the ledger)")
    ap.add_argument("--weeks", type=int, default=4)
    ap.add_argument("--include-current", action="store_true")
    ap.add_argument("--class", dest="klass",
                    choices=["guidance", "infra", "unclassified", "unlisted"])
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--no-header", action="store_true")
    ap.add_argument("-q", "--quiet", action="store_true")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--sample", metavar="CAUSE")
    ap.add_argument("-n", type=int, default=3)
    ap.add_argument("--since", type=int, default=28, metavar="DAYS")
    mode.add_argument("--list-causes", action="store_true")
    ap.add_argument("--fingerprint", type=pathlib.Path, metavar="FILE")
    ap.add_argument("--rebaseline", action="store_true")
    ap.add_argument("--helper", metavar="PATH")
    mode.add_argument("--self-test", action="store_true")
    return ap


def run(argv=None, now=None):
    ap = parser()
    args = ap.parse_args(argv)
    if args.self_test:
        return self_test()
    if args.rebaseline and args.fingerprint is None:
        ap.error("--rebaseline needs --fingerprint FILE")
    if (args.sample or args.list_causes) and args.fingerprint is not None:
        ap.error("--fingerprint applies to the weekly counts, not --sample or --list-causes")
    if args.list_causes and args.json:
        ap.error("--list-causes prints `cause class` lines; it has no --json")
    if args.weeks < 1 or args.n < 1 or args.since < 1:
        ap.error("--weeks, -n and --since take a positive number")
    if args.list_causes:
        for c, k, _ in CAUSES:
            if args.klass in (None, k):
                print(f"{c}\t{k}")
        return 0
    if args.sample and not valid_sample(args.sample):
        names = ", ".join([c for c, _, _ in CAUSES]
                          + [f"{r}:unclassified" for r in SPLIT] + ["<reason>:unlisted"])
        print(f"cause-counts: unknown cause `{args.sample}`; valid: {names}",
              file=sys.stderr)
        return 2
    if args.fingerprint is not None and not (args.helper or os.environ.get("MEASURE_FINGERPRINT")):
        print(NEED_HELPER, file=sys.stderr)
        return 2
    args.artifacts = args.artifacts or args.ledger.parent / "runs"
    now = now or dt.datetime.now(dt.timezone.utc)
    try:
        with contextlib.closing(open_ledger(args.ledger)) as con:
            if args.sample:
                sample(con, args, now)
                return 0
            rows, notes = count(con, args.artifacts, now.date(), args.weeks,
                                args.include_current)
    except Unreadable as err:
        print(f"cause-counts: {err}", file=sys.stderr)
        return 1
    except sqlite3.Error as err:
        print(f"cause-counts: cannot read the ledger {args.ledger} ({err}); "
              "try --ledger PATH", file=sys.stderr)
        return 1
    if args.fingerprint is not None:
        code = fingerprint(args, {"runs": notes["runs"], "weeks": args.weeks,
                                  "review_unknown": len(notes["unknown"])})
        if code is not None:
            return code
    elif not args.quiet:
        print("info: unfingerprinted (pass --fingerprint FILE to record the recipe)",
              file=sys.stderr)
    warn(notes, args.quiet)
    emit([r for r in rows if args.klass in (None, r["class"])], args)
    return 0


class _Failed(Exception):
    pass


def _eq(label, got, want, tally):
    tally[0] += 1
    if got != want:
        raise _Failed(f"{label}: expected {want!r}, got {got!r}")


def self_test():
    tally = [0]
    try:
        with tempfile.TemporaryDirectory() as scratch:
            _self_cases(pathlib.Path(scratch), tally)
    except _Failed as err:
        print(f"self-test failed: {err}", file=sys.stderr)
        return 4
    print(f"ok: {tally[0]} assertions", file=sys.stderr)
    return 0


def _self_cases(scratch_dir, tally):
    ledger = scratch_dir / "ledger.sqlite"
    con = sqlite3.connect(ledger)
    con.executescript(
        "create table runs (id text, repo_path text, created_at text);"
        "create table transitions (id integer primary key, run_id text, "
        "reason text, detail_json text, at text);")
    now = dt.datetime(2026, 11, 21, 12, tzinfo=dt.timezone.utc)  # a Saturday
    w1 = "2026-11-10T09:00:00+00:00"  # week of 2026-11-09
    cur = "2026-11-17T09:00:00+00:00"  # current week
    runs = {
        "a": ("/r/x", w1, [("worker_dispatched", {}), ("scope_exceeded", {})]),
        "b": ("/r/x-wt-y", w1, [("worker_dispatched", {}), ("verification_started", {}),
                                ("behavioral_failure", {"failures": ["t"]})]),
        "c": ("/r/x", w1, [("worker_dispatched", {}), ("verification_started", {}),
                           ("behavioral_failure", {"criteria": ["no test"]})]),
        "d": ("/r/x", w1, [("worker_dispatched", {}), ("verification_started", {}),
                           ("behavioral_failure", {"odd": 1})]),
        "e": ("/r/x", w1, [("worker_dispatched", {}), ("verification_started", {}),
                           ("verification_gap", {"gaps": [
                               "amont gate `x` is absent", "acceptance criterion `c`"]})]),
        "f": ("/r/x", w1, [("worker_dispatched", {}), ("verification_started", {}),
                           ("review_findings", {"detail": "two"})]),
        "g": ("/r/x", w1, [("worker_dispatched", {}), ("verification_started", {}),
                           ("review_repair", {}), ("checks_and_review_passed", {})]),
        "h": ("/r/x", w1, [("worker_dispatched", {}), ("verification_started", {}),
                           ("checks_and_review_passed", {})]),  # review skipped
        "i": ("/r/x", w1, [("worker_dispatched", {}), ("verification_started", {}),
                           ("checks_and_review_passed", {})]),  # clean review
        "j": ("/r/x", w1, [("blocked_preflight", {}), ("repair_exhausted", {})]),
        "k": ("/s/worktrees/replay/trial-1", w1, [("scope_exceeded", {})]),
        "m": ("/r/x", cur, [("worker_dispatched", {}), ("scope_exceeded", {})]),
        "n": ("/r/x", w1, [("worker_dispatched", {}), ("verification_started", {}),
                           ("review_findings", {})]),  # no events file
        "p": ("/r/x", w1, [("scope_exceeded", {})]),  # a candidate run, no worker
        "q": ("/r/x", w1, [("worker_dispatched", {}), ("verification_started", {}),
                           ("review_findings", {})]),  # findings, worker-only events
        "r": ("/r/x", "2026-11-10T09:00:00.123456789+00:00",  # nine fractional digits
              [("worker_dispatched", {}), ("verification_started", {}),
               ("verification_gap", {"gaps": ["verification setup did not complete"]})]),
        "s": ("/r/x", w1, [("worker_dispatched", {}), ("verification_started", {}),
                           ("verification_gap", {"gaps": "not a list"})]),
        "t": ("/r/x", "2026-11-10T09:00:00", [("scope_exceeded", {})]),  # naive time
    }
    events = {"f": "reviewer", "g": "reviewer", "i": "reviewer", "b": "worker",
              "c": "worker", "d": "worker", "e": "worker", "h": "worker",
              "q": "worker", "r": "worker", "s": "worker"}
    for rid, (repo, created, trs) in runs.items():
        con.execute("insert into runs values (?,?,?)", (rid, repo, created))
        for reason, detail in trs:
            con.execute("insert into transitions (run_id, reason, detail_json, at) "
                        "values (?,?,?,?)", (rid, reason, json.dumps(detail), created))
    con.commit()
    con.close()
    art = scratch_dir / "runs"
    for rid, kind in events.items():
        (art / rid).mkdir(parents=True)
        (art / rid / "events.jsonl").write_text(
            json.dumps({"event": {"kind": "dispatch_started", "agent_kind": kind}},
                       separators=(",", ":")) + "\n")
    ro = open_ledger(ledger)
    rows, notes = count(ro, art, now.date(), 4, True)
    by = {(r["week"], r["cause"]): r for r in rows}

    def cell(cause, key, week="2026-11-09"):
        return by[(week, cause)][key]

    _eq("weeks", sorted({r["week"] for r in rows}),
        ["2026-10-19", "2026-10-26", "2026-11-02", "2026-11-09", "2026-11-16"], tally)
    _eq("partial", cell("scope_exceeded", "partial", "2026-11-16"), "yes", tally)
    _eq("complete", cell("scope_exceeded", "partial"), "no", tally)
    _eq("replay excluded", cell("scope_exceeded", "runs"), 1, tally)
    _eq("dispatched", cell("scope_exceeded", "denominator"), 13, tally)
    _eq("current week", cell("scope_exceeded", "runs", "2026-11-16"), 1, tally)
    _eq("check_failed", cell("check_failed", "runs"), 1, tally)
    _eq("verified", cell("check_failed", "denominator"), 12, tally)
    _eq("criteria split", cell("criteria_unmet", "runs"), 1, tally)
    _eq("unclassified", cell("behavioral_failure:unclassified", "runs"), 1, tally)
    _eq("unclassified class", cell("behavioral_failure:unclassified", "class"),
        "unclassified", tally)
    _eq("gate_absent", cell("gate_absent", "runs"), 1, tally)
    _eq("criterion_unreported", cell("criterion_unreported", "runs"), 1, tally)
    _eq("review findings + repair", cell("review_findings", "runs"), 2, tally)
    _eq("reviewed (skipped review out)", cell("review_findings", "denominator"), 3, tally)
    _eq("review rate", cell("review_findings", "rate"), round(2 / 3, 4), tally)
    _eq("infra", cell("blocked_preflight", "runs"), 1, tally)
    _eq("created", cell("blocked_preflight", "denominator"), 15, tally)
    _eq("unlisted", cell("repair_exhausted:unlisted", "runs"), 1, tally)
    _eq("unlisted class", cell("repair_exhausted:unlisted", "class"), "unlisted", tally)
    _eq("zero denominator", cell("scope_exceeded", "rate", "2026-10-19"), None, tally)
    _eq("zero-row reasons", "same_failure_recurrence" in notes["zero"], True, tally)
    _eq("zero-row mapped only", "worker_dispatched" in notes["zero"], False, tally)
    _eq("review unknown", notes["unknown"], ["n"], tally)
    _eq("findings without a reviewer dispatch", notes["unreviewed"], ["q"], tally)
    _eq("setup_incomplete", cell("setup_incomplete", "runs"), 1, tally)
    _eq("nine-digit fraction kept", cell("setup_incomplete", "denominator"), 12, tally)
    _eq("gaps not a list: one row", cell("verification_gap:unclassified", "runs"), 1, tally)
    _eq("naive created_at left out", notes["bad_time"], ["t"], tally)
    _eq("outside its stage", notes["outside"].get("scope_exceeded"), ["p"], tally)
    _eq("outside: not reviewed", sorted(notes["outside"].get("review_findings")),
        ["n", "q"], tally)
    _eq("review runs (unknown n not unreviewed)", notes["review_runs"], 4, tally)
    _eq("guidance list", [c for c, k, _ in CAUSES if k == "guidance"],
        ["scope_exceeded", "check_failed", "criteria_unmet", "criterion_unreported",
         "same_failure_recurrence", "review_findings"], tally)
    _eq("sample valid", valid_sample("verification_gap:unclassified"), True, tally)
    _eq("sample invalid", valid_sample("nonsense"), False, tally)


def main():
    try:
        code = run()
        sys.stdout.flush()
        return code
    except BrokenPipeError:
        try:
            os.dup2(os.open(os.devnull, os.O_WRONLY), sys.stdout.fileno())
        except OSError:  # stdout is already gone; nothing is left to tell
            pass
        return 0
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())
