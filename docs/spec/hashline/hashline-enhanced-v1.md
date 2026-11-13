# Hashline Enhanced v1

Status: implemented as an explicit edit style. The default style is `anchor`.
Identifier: `hashline-enhanced`. Contract version: 1.
Required base: [Shared specification](hashline-common-v1.md).

## E01. Purpose

Preserve observed-target editing while removing avoidable reference churn,
short-tag collisions, duplicate-section rejection, no-op errors, replayed
execution, and unnecessary context-repair round trips.

Enhanced maps untouched original targets only through authoritative recorded
operations. It does not relocate automatically through arbitrary external edits.
Unique equal text in another location is not occurrence identity.

## E02. Acceptance condition

For each effective replacement or CUT:

```text
EnhancedEligible(edit) = SharedChecks(edit) AND PriorCoverage(edit)
```

Every existing cell in the effective destructive footprint MUST have been fully
delivered to this caller before its request cutoff. Coverage belongs to the
same snapshot/history and caller and transfers only through exact preserved
cells. Host-internal reads, other agents' reads, truncated output, and future
previews cannot satisfy it.

For the same effective destructive operation, Enhanced eligibility is a subset
of Light eligibility. Only prior coverage differs. Permission, target protection,
resource rules, and commit behavior do not.

Each thread keeps its own snapshot store. Each read mints a new reference, so a patch counts only rows delivered under its own reference.

## E03. Explicit exceptions

NEW removes no existing content and is guarded by absence. Pure-gap insertion
needs identity but no removed-line coverage. When newline preservation changes an
existing cell, that cell belongs to the footprint and requires coverage.

REM retains the source plan's explicit-file-delete exception in both profiles.
It requires an unchanged complete source reference and normal authorization, but
not every line delivered. Do not advertise Enhanced as preventing every deletion
of unseen text. Explicit REM is an intentional exception.

A valid exact no-op has no effective footprint and returns no_change after
reference/target validation. This is not a guess that a previous call already ran.

The parser refuses source-node (`N*`) operations with a parse error.

## E04. Examples

Snapshot S includes fully delivered source lines 40-45. A cooperating patch inserts
ten lines before line 5. The host maps S:40-45 to current 50-55 and can prepare the
edit without another whole-file read.

If a cooperating patch inserts inside the target, it conflicts: the new middle
text is not in the requested source span. If the target changes and later returns
to equal bytes, it remains invalid. If the current file breaks retained history,
require a new read rather than searching for an equal block elsewhere.

A model request starts at delivery cutoff 7 and emits calls A and B. A fails and
returns a preview at delivery 8. B cannot use delivery 8 because B is already
chosen under cutoff 7. A subsequent request with cutoff 8 MAY use complete rows
from that preview. This establishes availability to a decision context, not proof
of comprehension.

Sequential evaluators follow their separate ordered return boundary. Their read
results do not count as model observations unless separately delivered there.

## E05. Recovery

For a known target with unseen rows, the request fails with this error:
`patch: changes[N]: lines A-B of PATH need prior observation. Read them, then resend`.
The request writes no target. A later explicit request MAY use the new delivery.
Never upgrade the pending request after an internal fetch.

For missing history, the request fails with `patch: reference REF for PATH is stale: snapshot: stale reference. Read again`.
The caller MUST read the file again. Do not offer a retag-only repair that silently reuses old unverified coordinates.

## E06. Canonical model-facing description

The description is `enhancedDescription` in `internal/tool/patch/description.go`:

```text
Edit files from a read/search snapshot. Copy its [path@reference] header.
PUT N: or PUT N-M: replaces original lines using + body rows. CUT removes
original lines. PUT <N:, PUT >N:, and PUT >$: insert in source-snapshot
gaps. Every existing cell
changed by replacement or CUT must have been fully shown to this caller.
Folded/truncated rows do not count. A missing-context reply can supply rows
for a later request, never this one. The host maps intact targets through
known edits. Equal text elsewhere is not the target; missing history needs
a fresh read. Known changed targets stay invalid even if old text returns.
[PATH#NEW] creates. REM explicitly deletes under unchanged full-file
reference and approval; REM is exempt from the every-line display rule.
MV DEST renames after edits. Do not mix references or overlap operations.
A valid no-op is not an error or proof of task completion. Never downgrade
to Light to bypass refusal. Never blind-retry an uncertain commit.
```

## E07. Acceptance cases

| ID | Given | Required result |
|---|---|---|
| E-01 | Fully observed intact target before cutoff | Preparation succeeds. |
| E-02 | Folded or truncated middle rows | Error ends with `need prior observation. Read them, then resend`. No target is written. |
| E-03 | Complete missing rows delivered before a new request | A new request can succeed. |
| E-04 | Missing rows delivered after this request began | Pending call still lacks coverage. |
| E-05 | Another subagent observed the same bytes | No coverage for this consumer. |
| E-06 | Intact target moved through known prefix insertion | Map and prepare. |
| E-07 | Original changes while equal text survives elsewhere | Reject; never retarget. |
| E-08 | Known edit-then-undo restores equal bytes | Reject; no revival. |
| E-09 | Concurrent insertion occupies the same gap | Error ends with `is stale: snapshot: gap changed. Read again`. No target is written. |
| E-10 | Source-node operation `N*` | Error: `patch: line N: block ops (N*) are not supported; use a line range`. |
| E-11 | REM, unchanged full-file reference, approval, partial delivery | Eligible under explicit REM exception. |
| E-12 | Valid no-op on unseen body | no_change; no invented coverage. |
| E-13 | EOF edit changes an unobserved neighbor's terminator | Missing-coverage refusal for actual footprint. |
| E-14 | Replay returns a historical preview | No fresh reference or observation credit. |

All common conformance tests also apply.
