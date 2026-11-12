# Hashline Light v1

Status: implemented as an explicit edit style. The default style is `anchor`.
Identifier: `hashline-light`. Contract version: 1.
Required base: [Shared specification](hashline-common-v1.md).

## L01. Purpose

Light permits deliberate replacement of text the caller did not fully inspect.
It keeps snapshot binding and concurrent-change detection.

Light is snapshot-bound, observation-optional editing. It is not a write to
whatever currently occupies an old line number. Missing references, changed
targets, broken history, stale approval, or invalid authority still prevent writes.

## L02. Acceptance condition

For each effective replacement or CUT:

```text
LightEligible(edit) = SharedChecks(edit)
```

SharedChecks includes eligible caller-bound reference, valid source locator,
continuous verified history, unchanged target, resource admission, containment,
nonoverlap, staged-observer acceptance, and authorization. Eligibility permits
preparation. It does not guarantee I/O success.

Light MUST NOT require every affected line to have been delivered. The complete source
snapshot and mapping evidence MUST still be available, and its header MUST have
been delivered before the call's cutoff. Missing display is permitted. Missing
snapshot evidence is not.

## L03. Example

A read captures a complete file snapshot but delivers:

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

Syntax, mapping, gaps, NEW, REM, MV, byte preservation, output, replay, and
limits follow the common contract. The parser refuses `N*` operations with `patch: line N: block ops (N*) are not supported; use a line range`.

REM retains the explicit-file-delete observation exemption common to both profiles.
It still requires an unchanged complete source reference and normal authorization.
A valid no-op returns no_change without file writes or fabricated coverage. Invalid
or stale requests cannot become no-ops merely because desired text already exists.

Light can remove important code the caller never inspected. That is its deliberate
risk. Snapshot validity does not establish that the chosen edit is appropriate.

Only user-controlled configuration or runtime policy established before the model
request selects Light. A model cannot select it by changing its header. A refused
Enhanced edit MUST NOT fall back to Light automatically. No unmeasured model-family fallback
table is part of this specification.

## L05. Canonical model-facing description

The description is `lightDescription` in `internal/tool/patch/description.go`:

```text
Edit files from a read/search snapshot. Copy its [path@reference] header.
PUT N: or PUT N-M: replaces original lines using + body rows. CUT N or
CUT N-M removes them. PUT <N:, PUT >N:, and PUT >$: insert in original
snapshot gaps.
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

## L06. Acceptance cases

| ID | Given | Required result |
|---|---|---|
| L-01 | Delivered reference, retained snapshot, unseen intact range | Preparation succeeds. |
| L-02 | Same range after unrecorded external drift | Error ends with `is stale: snapshot: stale reference. Read again`. No target is written. |
| L-03 | Recorded insertion before an intact range | Map original range and prepare. |
| L-04 | Recorded insertion inside that range | Error ends with `is stale: snapshot: target changed. Read again`. No target is written. |
| L-05 | Source-node operation `N*` | Error: `patch: line N: block ops (N*) are not supported; use a line range`. |
| L-07 | Tagless existing-file header | No current-coordinate write. |
| L-08 | Target valid but approval declined | Decline with reason `declined by user`. |
| L-09 | Target changed then restored through known history | Conflict despite equal final bytes. |
| L-10 | Valid no-op with no coverage | no_change in both profiles. |

All common conformance tests also apply.
