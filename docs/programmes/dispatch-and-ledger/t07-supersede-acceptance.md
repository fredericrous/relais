+++
type = "task"
mode = "hitl"
status = "closed"
blocked_by = ["t03-attempt-ends-on-amont", "t05-review-leaves-relais", "t08-when-a-baseline-runs"]
repos = ["relais"]
+++
# Record relais's two acceptance adapters, the reviewer's removal and the baseline rule

## Question

The relais ADR that records: acceptance through greengate when present and
a plain run otherwise, the semantic reviewer made opt-in (t05), and the
baseline rule (t08), together with the SPEC sections and ADRs t01 lists as affected.
What is the record, and does the person accept it?

## Resolution

The person, 2026-11-10: **"Accept as drafted"**, chosen over also amending
SPEC now and over changing the wording.

- **The record:** [ADR-0013](../../../.adr/records/0013-acceptance-has-two-adapters-review-is-opt-in.md),
  "acceptance has two adapters, review is opt-in, a baseline runs only when
  needed". It adds three keys to `.adr.yaml`:
  - `acceptance.adapters`: with amont, a profile gate runs as `amont run
    --stamp <gate>`; without it, the commands run plainly; relais never
    requires amont.
  - `acceptance.semantic-review`: the reviewer runs only under `review:
    required`, and its two guards come with it ([t05](t05-review-leaves-relais.md)).
  - `acceptance.baseline`: proof, else lazy ([t08](t08-when-a-baseline-runs.md)).
- It is numbered 0013 because an unmerged ADR-0012 (a local-model worker)
  sits on `feat/local-model-rung`.
- **SPEC and the older ADRs:** SPEC §4, §5, §6, §9, §10, §11, §17, §18 and
  §23, ADR-0004, ADR-0006, ADR-0010, ADR-0011, and decisions ADR-0045 are
  amended by each build PR that changes the behaviour, not now. So SPEC
  never describes unbuilt behaviour.
- **What it gives up:** the ADR's Consequences state the three costs.
  - An unreviewed `inspect` run is verified only by its report existing.
  - A weakened test is caught only at F4b and at push.
  - A red base is caught only at the first red candidate.
