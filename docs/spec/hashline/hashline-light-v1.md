# Hashline Light v1

Status: reviewed proposal; not implemented or selected by default.
Identifier: `hashline-light`. Contract version: 1.
Required base: [Shared specification](hashline-common-v1.md).

## L01. Purpose

Permit deliberate replacement of text the caller did not fully inspect, while
retaining snapshot binding and concurrent-change detection.

Light is snapshot-bound, observation-optional editing. It is not a write to
whatever currently occupies an old line number. Missing references, changed
targets, broken history, stale approval, or invalid authority still prevent writes.

## L02. Acceptance condition

For each effective replacement or CUT, including source-node operations:

```text
LightEligible(edit) = SharedChecks(edit)
```

SharedChecks includes eligible caller-bound reference, valid source locator,
continuous verified history, unchanged target, resource admission, containment,
nonoverlap, staged-observer acceptance, and authorization. Eligibility permits
preparation; it does not guarantee I/O success.

Do not require every affected line to have been displayed. The complete source
snapshot and mapping evidence must still be available, and its header must have
been delivered before the call's cutoff. Missing display is permitted; missing
snapshot evidence is not.

## L03. Example

A read captured a complete file snapshot but displayed:

```text
[src/table.rs@r0123456789abcdef0123456789abcdef.1]
100: fn generated_table() {
     ... 79 source lines omitted ...
180: }
```

This request is eligible in Light when that original range survives unchanged:

```text
[src/table.rs@r0123456789abcdef0123456789abcdef.1]
PUT 100-180:
+fn generated_table() {
+    build_table()
+}
```

The omitted rows never become observed. Light simply waives their coverage check.
A recorded change at source line 140 conflicts. An unrecorded external mutation
ends the usable history even if the old endpoint text survives elsewhere.

## L04. Operations, exceptions, and selection

Syntax, mapping, gaps, N*, NEW, REM, MV, byte preservation, output, replay, and
limits follow the common contract unchanged.

REM retains the explicit-file-delete observation exemption common to both profiles.
It still requires an unchanged complete source reference and normal authorization.
A valid no-op returns no_change without file writes or fabricated coverage. Invalid
or stale requests cannot become no-ops merely because desired text already exists.

Light may remove important code the caller never inspected. That is its deliberate
risk. Snapshot validity does not establish that the chosen edit is appropriate.

Only user-controlled configuration or runtime policy established before the model
request selects Light. A model cannot select it by changing its header. Never
downgrade an Enhanced refusal automatically. No unmeasured model-family fallback
table is part of this specification.

## L05. Canonical model-facing description

The symbols-on description is:

```text
Edit files from a read/search snapshot. Copy its [path@reference] header.
PUT N: or PUT N-M: replaces original lines using + body rows. CUT N or
CUT N-M removes them. PUT <N:, PUT >N:, and PUT >$: insert in original
snapshot gaps. N* operations require enabled symbol support.
[PATH#NEW] plus PUT >$: creates a file. REM explicitly deletes; MV DEST
renames after edits. REM/MV require the unchanged complete source file.
All operations use the referenced before-image. Light permits replacement
of lines not fully displayed, but rejects changed targets, missing history,
invalid paths and stale approval. Known unrelated edits can move an untouched
target; the host maps them. Unknown edits require a new read. Never invent
a reference or choose a target by similar text. Empty PUT is invalid; use
CUT. A valid no-op is not an error or proof of task completion. Pre-commit
refusal writes no target. Inspect commit I/O outcomes; never retry blindly.
```

With symbols off, remove the N* sentence and its grammar productions. Keep all
other semantics identical. Shared quoting and byte-framing rules still apply.

## L06. Acceptance cases

| ID | Given | Required result |
|---|---|---|
| L-01 | Delivered reference, retained snapshot, unseen intact range | Preparation succeeds. |
| L-02 | Same range after unrecorded external drift | needs_context/history_gap; no write. |
| L-03 | Recorded insertion before an intact range | Map original range and prepare. |
| L-04 | Recorded insertion inside that range | conflict/target_changed. |
| L-05 | Unseen source node with valid source parse and preserved extent | Preparation succeeds. |
| L-06 | Source-node evidence expired | needs_context; no current-AST guess. |
| L-07 | Tagless existing-file header | No current-coordinate write. |
| L-08 | Target valid but authorization denied | rejected/permission_denied. |
| L-09 | Target changed then restored through known history | Conflict despite equal final bytes. |
| L-10 | Valid no-op with no coverage | no_change in both profiles. |

All common conformance tests also apply.
