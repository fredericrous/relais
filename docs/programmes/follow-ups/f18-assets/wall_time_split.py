#!/usr/bin/env python3
"""f18: where a timed-out relais worker's wall time goes.

Matches each timed-out dispatch to its subagent transcript the way f15 did
(ledger session_id; the subagent file whose first line is within 2 min of
dispatch_started), then splits dispatch_started..dispatch_ended into:

  spawn   dispatch_started -> first transcript line
  model   tool_result (or start) -> next assistant line, and the tail
  suite   Bash running a test / build / lint command (foreground)
  poll    sleep / tail -f / wait loops, BashOutput/TaskOutput/Monitor
  late    the part of poll time after a finish marker was already seen
  other   every other tool call
  killed  the call still in flight at the kill (counted in its class too)

Tool time is the union of call intervals, so parallel calls do not double
count; each instant goes to the highest class (suite > poll > other).

Host signals per run: concurrent relais dispatches (ledger), load1 from
~/.local/state/laptop-load/samples.tsv (Nov 1-17), greengate slot waits from
/tmp/greenharness-slots-<uid>/waits.jsonl (Nov 17-), and waiting_for_slot
events of every run during the window.

Usage: python3 -I wall_time_split.py [--json out.json]
"""
import bisect, datetime as dt, glob, json, os, re, sqlite3, statistics, sys

HOME = os.path.expanduser("~")
RUNS = f"{HOME}/.local/state/relais/runs"
LEDGER = f"{HOME}/.local/state/relais/ledger.sqlite"
PROJECTS = f"{HOME}/.claude/projects"
SAMPLES = f"{HOME}/.local/state/laptop-load/samples.tsv"
WAITS = f"/tmp/greenharness-slots-{os.getuid()}/waits.jsonl"


def ts(s):
    return dt.datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


SUITE = re.compile(
    r"\b(cargo\s+(\+\S+\s+)?(test|build|clippy|check|nextest|run|doc|bench)"
    r"|pytest|vitest|jest|playwright|tsc\b|go\s+test|make\b|just\b"
    r"|(npm|pnpm|yarn|npx|bun)\s+(-\S+\s+|--cwd\s+\S+\s+|workspace\s+\S+\s+)*(run\s+)?(test|build|lint|check|typecheck|tsc|vitest|exec|verify)"
    r"|greengate\s+(run|land|rehearse|slot\s+run)|amont\s+(land|slot\s+run)|uv\s+run|storybook|xcodebuild|swift\s+(build|test))"
)
POLL_CMD = re.compile(r"\bsleep\s+\d|tail\s+(-n\s*\+?\d+\s+)?-[a-zA-Z]*[fF]|(^|[;&|(]\s*)wait(\s|;|$)|\buntil\b|while\s+.*;\s*do|timeout\s+\d+\s+tail")
POLL_TOOLS = {"BashOutput", "TaskOutput", "Monitor", "TaskGet", "KillShell"}
FINISH = re.compile(
    r"test result:|Finished `|Finished \w+ \[|clippy_done|\bpassed\b.*\bin\b|Tests:\s+\d+|"
    r"exit code|exited with|status: completed|<status>completed|\bDONE\b|ALL_DONE"
)
NO_FILE = re.compile(r"No such file|cannot open|\(Bash completed with no output\)")
CARGO_FIN = re.compile(r"Finished `?\w+`? profile.*? in ([\d.]+m? ?[\d.]*s)")


def dur_s(txt):
    m = re.match(r"(?:([\d.]+)m)?\s*([\d.]+)s", txt)
    if not m:
        return None
    return float(m.group(1) or 0) * 60 + float(m.group(2))


def text_of(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(b.get("text", "") for b in content if isinstance(b, dict))
    return ""


def timed_out_dispatches():
    out = []
    for d in sorted(glob.glob(f"{RUNS}/run-*")):
        ev = f"{d}/events.jsonl"
        if not os.path.exists(ev):
            continue
        starts, ends = {}, {}
        timed = False
        for line in open(ev):
            try:
                e = json.loads(line)
            except ValueError:
                continue
            k = e["event"].get("kind")
            if k == "dispatch_started":
                starts[e["event"]["dispatch"]] = (ts(e["at"]), e["event"])
            elif k == "dispatch_ended":
                ends[e["event"]["dispatch"]] = (ts(e["at"]), e["event"])
            elif k == "phase" and (e["event"].get("detail") or {}).get("timed_out"):
                timed = True
        if not timed:
            continue
        for did, (t1, ee) in ends.items():
            if ee.get("outcome") == "timed out" and did in starts:
                out.append((os.path.basename(d), did, starts[did][0], t1, starts[did][1]))
    return out


def find_transcript(session, t0):
    dirs = glob.glob(f"{PROJECTS}/*/{session}/subagents")
    best = None
    for sd in dirs:
        for f in glob.glob(f"{sd}/*.jsonl"):
            try:
                first = json.loads(open(f).readline())
                t = ts(first["timestamp"])
            except Exception:
                continue
            if 0 <= t - t0 <= 120 and (best is None or t - t0 < best[0]):
                best = (t - t0, f)
    return best[1] if best else None


def classify_use(name, inp):
    if name in POLL_TOOLS:
        return "poll"
    if name == "Bash":
        cmd = inp.get("command", "")
        if inp.get("run_in_background"):
            return "launch"
        if POLL_CMD.search(cmd):
            return "poll"
        if SUITE.search(cmd):
            return "suite"
    return "other"


RANK = {"suite": 3, "poll": 2, "other": 1, "launch": 1}


def analyse(path, t0, t1):
    uses, results, asst_times, user_times = {}, {}, [], []
    lines = []
    for line in open(path):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if "timestamp" not in e:
            continue
        t = ts(e["timestamp"])
        lines.append(t)
        c = (e.get("message") or {}).get("content")
        if e.get("type") == "assistant":
            asst_times.append(t)
            if isinstance(c, list):
                for b in c:
                    if b.get("type") == "tool_use":
                        uses[b["id"]] = (t, b.get("name"), b.get("input") or {})
        elif e.get("type") == "user" and isinstance(c, list):
            for b in c:
                if b.get("type") == "tool_result":
                    results[b["tool_use_id"]] = (t, text_of(b.get("content")))
                    user_times.append(t)
    first = min(lines) if lines else t0
    calls = []
    finish_seen_at = None
    for uid, (tu, name, inp) in sorted(uses.items(), key=lambda kv: kv[1][0]):
        cls = classify_use(name, inp)
        tr, txt = results.get(uid, (None, ""))
        killed = tr is None
        end = min(tr if tr else t1, t1)
        calls.append(dict(start=tu, end=end, cls=cls, name=name, killed=killed,
                          cmd=(inp.get("command") or inp.get("description") or name)[:160],
                          out=txt))
    # late polls: poll time after a finish marker already appeared in an earlier
    # poll result or background-task notification
    events = sorted([(c["end"], "res", c) for c in calls])
    for c in sorted(calls, key=lambda c: c["start"]):
        c["late"] = False
        if c["cls"] == "launch":
            finish_seen_at = None
        if c["cls"] == "poll" and finish_seen_at is not None and finish_seen_at <= c["start"]:
            c["late"] = True
        if c["cls"] in ("poll", "launch") and FINISH.search(c["out"] or ""):
            finish_seen_at = c["end"] if finish_seen_at is None else min(finish_seen_at, c["end"])
        c["nofile"] = c["cls"] == "poll" and bool(NO_FILE.search((c["out"] or "")[:400]))
    # union of intervals by class priority
    last = max(lines) if lines else t0
    last = min(t1, max(last, max([c["end"] for c in calls], default=last)))
    pts = sorted({t0, t1, first, last} | {c["start"] for c in calls} | {c["end"] for c in calls})
    pts = [p for p in pts if t0 <= p <= t1]
    tot = dict(spawn=max(0.0, min(first, t1) - t0), tail=max(0.0, t1 - last), model=0.0, suite=0.0, poll=0.0,
               late=0.0, nofile=0.0, other=0.0, killed=0.0)
    for a, b in zip(pts, pts[1:]):
        if b <= first:
            continue
        a = max(a, first)
        mid = (a + b) / 2
        active = [c for c in calls if c["start"] <= mid < c["end"]]
        if not active:
            if a >= last:
                continue  # the tail, counted above
            tot["model"] += b - a
            continue
        top = max(active, key=lambda c: RANK[c["cls"]])
        k = "other" if top["cls"] == "launch" else top["cls"]
        tot[k] += b - a
        if k == "poll" and any(c["late"] for c in active if c["cls"] == "poll"):
            tot["late"] += b - a
        if k == "poll" and any(c["nofile"] for c in active if c["cls"] == "poll"):
            tot["nofile"] += b - a
        if any(c["killed"] for c in active):
            tot["killed"] += b - a
    cargo_fin = []
    for c in calls:
        for m in CARGO_FIN.finditer(c["out"] or ""):
            d = dur_s(m.group(1))
            if d:
                cargo_fin.append(d)
    longest = sorted(calls, key=lambda c: c["end"] - c["start"], reverse=True)[:3]
    return tot, len(calls), longest, cargo_fin


def load_samples():
    xs, ys, cpu = [], [], {}
    if not os.path.exists(SAMPLES):
        return xs, ys, cpu
    for line in open(SAMPLES):
        p = line.split("\t")
        if p[0] == "load" and len(p) >= 4:
            xs.append(int(p[1])); ys.append(float(p[3]))
        elif p[0] == "cpu" and len(p) >= 4:
            cpu.setdefault(int(p[1]), {})[p[2]] = float(p[3])
    return xs, ys, cpu


def mean_in(xs, ys, a, b):
    i, j = bisect.bisect_left(xs, a), bisect.bisect_right(xs, b)
    v = ys[i:j]
    return (statistics.mean(v), max(v), len(v)) if v else None


def main():
    db = sqlite3.connect(f"file:{LEDGER}?mode=ro", uri=True)
    disp = {r[0]: r[1:] for r in db.execute(
        "select dispatch_id, session_id, created_at, updated_at, run_id from dispatches")}
    spans = [(ts(r[1]), ts(r[2]), k, r[3]) for k, r in disp.items() if r[1] and r[2]]
    xs, ys, cpu = load_samples()
    waits = []
    if os.path.exists(WAITS):
        for line in open(WAITS):
            try:
                waits.append(json.loads(line))
            except ValueError:
                pass
    # waiting_for_slot events of every run, for host saturation
    wfs = []
    for d in glob.glob(f"{RUNS}/run-*/events.jsonl"):
        for line in open(d):
            if '"waiting_for_slot"' in line:
                e = json.loads(line)
                wfs.append((ts(e["at"]), e["event"].get("class"), e["event"].get("waited_s", 0)))
    wfs.sort()
    rows = []
    for run, did, t0, t1, ev in timed_out_dispatches():
        session = (disp.get(did) or (None,))[0]
        path = find_transcript(session, t0) if session else None
        row = dict(run=run, dispatch=did, worker=ev.get("model"), kind=ev.get("agent_kind"),
                   start=dt.datetime.fromtimestamp(t0, dt.timezone.utc).isoformat(timespec="minutes"),
                   wall=t1 - t0, transcript=bool(path))
        # concurrency: time-weighted other relais dispatches in the window
        conc = 0.0
        for a, b, k, _ in spans:
            if k == did:
                continue
            ov = min(b, t1) - max(a, t0)
            if ov > 0:
                conc += ov
        row["concurrent"] = conc / (t1 - t0)
        lo = mean_in(xs, ys, t0, t1)
        row["load1_mean"], row["load1_max"] = (lo[0], lo[1]) if lo else (None, None)
        if lo:
            ks = [k for k in cpu if t0 <= k <= t1]
            agg = {}
            for k in ks:
                for g, v in cpu[k].items():
                    agg[g] = agg.get(g, 0) + v
            row["cpu_groups"] = {g: round(v / len(ks), 1) for g, v in agg.items()} if ks else None
        w = [x for x in waits if t0 * 1000 <= x.get("at_ms", 0) <= t1 * 1000]
        row["slot_waits_s_host"] = sum(x.get("waited_ms", 0) for x in w) / 1000 if w else None
        row["slot_waits_s_own"] = (sum(x.get("waited_ms", 0) for x in w if x.get("session") == session) / 1000) if w else None
        i, j = bisect.bisect_left(wfs, (t0,)), bisect.bisect_right(wfs, (t1, "~"))
        row["wfs_events"] = j - i
        if path:
            tot, n, longest, cf = analyse(path, t0, t1)
            row.update(tot); row["calls"] = n
            row["cargo_finished_s"] = round(sum(cf))
            row["longest"] = [dict(s=round(c["end"] - c["start"]), cls=c["cls"], killed=c["killed"],
                                   at=dt.datetime.fromtimestamp(c["start"], dt.timezone.utc).isoformat(timespec="minutes"),
                                   cmd=c["cmd"]) for c in longest]
        rows.append(row)
    if "--json" in sys.argv:
        json.dump(rows, open(sys.argv[sys.argv.index("--json") + 1], "w"), indent=1)
    found = [r for r in rows if r["transcript"]]
    print(f"timed-out dispatches {len(rows)}, transcripts {len(found)}")
    keys = ["spawn", "tail", "model", "suite", "poll", "late", "nofile", "other", "killed"]
    W = sum(r["wall"] for r in found)
    print(f"wall {W:.0f}s; " + ", ".join(f"{k} {sum(r[k] for r in found):.0f}s ({100*sum(r[k] for r in found)/W:.0f}%)" for k in keys))
    for r in sorted(found, key=lambda r: r["run"]):
        print(r["run"][4:17], r["worker"], r["start"], f"wall={r['wall']:.0f}",
              " ".join(f"{k}={r[k]:.0f}" for k in keys), f"calls={r['calls']}",
              f"conc={r['concurrent']:.1f}", f"load={r['load1_mean'] and round(r['load1_mean'],1)}",
              f"wfs={r['wfs_events']}", f"slotwait_own={r['slot_waits_s_own']}", f"cargoFin={r['cargo_finished_s']}")


if __name__ == "__main__":
    main()
