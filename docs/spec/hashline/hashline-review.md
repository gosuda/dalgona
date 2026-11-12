# Hashline v1: review and amendment record

Date: 2026-09-28.
Status: reviewed specification proposal. No product implementation or default was
changed. The uploaded plan was not edited.

## R01. Intent and scope

The request is to review the preceding design, revise it, and write specifications.
This package supplies a shared contract, two profiles, conformance requirements,
and reproducible bounded model evidence. It does not implement the product.

| Interpretation | Weight | Falsifier |
|---|---:|---|
| Write reviewed specification artifacts from the current proposal and attached plan | 0.90 | Explicit instruction to implement repository code now. |
| Implement and publish the change immediately | 0.10 | The request asks to write specs, and the attached plan keeps planning private. |

These weights concern task interpretation, not correctness confidence. The first
governs this package. No question blocks writing. Live interface validation remains
an implementation gate, not something inferred from a filename.

## R02. Source map

Primary source: attached `spec/dalgon-v0-plan.md`, kept in this repository at `dalgona-private/spec/dalgon-v0-plan.md`, 16,732 physical lines, 2,107,077 bytes.
SHA-256, verified against the mounted attachment:

```text
dba503f39f66da0717fb0bf66b4bf77f93d6334e9ac6a755ce4d240749e7ca52
```

This package does not redistribute another copy of that private source. Locators
below refer to its physical lines, not re-rendered tool-response line numbers.

| Locator | Source lines | Supported content |
|---|---|---|
| S-OWNER | 5340-5358 | Patch ownership, shared IR, read/search and provider-selected dialect interfaces. |
| S-ENGINE | 5359-5412 | Parser/engine separation, observers, staging and output. |
| S-PROOF | 5414-5432 | Before-image resolution, raw bytes, Seen, Chain, proof requirements and caps. |
| S-WRITE | 5434-5443 | Approval recheck, staging, rollback, crash and external-race limitations. |
| S-CONFIG | 5445-5475 | Style values, aliases, model selection and anchor default. |
| S-GRAMMAR | 5507-5565 | Four-hex Hashline header; PUT/CUT/gaps/REM/MV and body syntax. |
| S-AST | 5583-5589 | Existing parser reuse, node addressing and mapped-coordinate checks. |
| S-OUTPUT | 5593-5702 | Echoes, stale/unknown tags, no-op errors, duplicate sections and I/O reports. |
| S-TEST | 5704-5810 | Existing proof, byte, ambiguity, write and style acceptance tests. |
| S-PRIVATE | 106-112; 622-628 | Private planning/public product boundary. |

The conversation proposals are design inputs, not implementation evidence.
Earlier assistant safety rankings, success-rate predictions, and a 15,048-case
claim were not treated as verified results. Fresh bounded evidence is included.

The source references other architecture and decision documents not separately
provided. No claim is made that every external cross-reference or live repository
API has been verified. This review is bounded to the attached patch-related
contract and the preceding proposals.

## R03. Findings and revisions

### Critical: equal content can identify the wrong occurrence

The first Enhanced draft proposed unique exact line/node matching after arbitrary
external edits. The original target can change while a copy of its old text
survives elsewhere. Every digest can match and the selected occurrence can still
be wrong.

Revision: executable mapping uses only authoritative recorded operations.
Content search is diagnostic, never edit proof. Covered by C02/C06-C09 and MAP-07.

### High: known edit-then-undo must not revive a target

A current-digest fast path can hide A-to-B-to-A history. Likewise, an insertion
inside a protected range can be removed later, restoring adjacency but not the
validity of the original request.

Revision: revisions and epochs are distinct from digests. Validate history first.
A known target conflict remains terminal. C06-C08 state the rule. REF-06, MAP-05 and MAP-06 test it.

### High: new information cannot authorize an already-made decision

Internal rereads or error previews could otherwise let a pending patch satisfy
its own missing coverage. Parallel calls from one model response are especially
vulnerable to this timing error.

Revision: evidence is consumer-scoped and frozen at request dispatch. Later
complete deliveries can authorize only later decisions. Evaluators get their own
ordered-return boundary. This is availability, not a claim of comprehension.
C05/E04 state the rule. OBS-02 through OBS-12 test it.

### High: effective footprints include raw newline changes

Appending after an unterminated final line changes its terminator. Deleting the
last unterminated line can change its predecessor. A pure line-coordinate model
that ignores this may preserve false evidence for an affected cell.

Revision: expand raw mutation footprints before proof and history recording.
If no sound line-cell splice exists, end history rather than invent a narrow map.
C11 states the rule. RAW-03 through RAW-05 test it.

### High: references need lifetime and consumer binding

A reusable r17 alone cannot distinguish restart, eviction, resume, and another
consumer. Short content hashes also do not identify document revision history.

Revision: opaque boot-plus-counter references with complete internal binding,
non-reuse, and explicit expiry. They identify evidence, not permissions. The
longer reference is a deliberate cost to measure, not a claimed reference reduction.
C04-C05 state the rule. REF-01 through REF-06 test it.

### High: Light was weaker than necessary

The early tagless Light trusted whatever currently occupied old line numbers.
That discarded target protection, not just observation requirements.

Revision: both profiles remain snapshot-bound. Light alone waives prior coverage
for effective destructive edits. Changed targets still reject. C01 states the rule. L02/E02 test it.

### High: rollback and replay guarantees were overstated

The original writer attempts rollback but explicitly permits restore failures,
crash uncertainty, and races with non-cooperating processes. A provider CallId
alone also cannot safely identify a replay across requests.

Revision: private prepared plans, post-approval validation, explicit installed/
restored/partial/unknown outcomes, runtime invocation identity, and existing-journal
start/settled records. Unsettled recovery is indeterminate, not authority to run
again. C12-C16 state the rule. IO and REP tests check it. No exactly-once filesystem guarantee is claimed.

### Medium: memory budgets need host-wide accounting

Eight MiB per thread would allow about 3.9 GiB across 500 threads before other
allocations. A shared pool also needs limits on metadata, pins, staging, and
batches that cannot fit.

Revision: proposed 32 MiB retained-evidence and 64 MiB host-wide staging budgets.
All-or-none lease admission, no partial-pin waits or hidden copies. Successful
writes remain successful when only fresh-reference retention fails. C14 states the rule. MEM tests check it.
These are allocation budgets, not measured total-RSS results.

The current code limits each call to 64 MiB of staged content. Each thread snapshot store holds at most 512 entries and 64 MiB.

### Medium: observation claims need lifecycle exceptions

The source explicitly exempts whole-file Delete from its ordinary removed-line
proof rule. Saying Enhanced never deletes unseen content would contradict that
exception. NEW/MV/no-op behavior also needed definitions.

Revision: preserve and disclose explicit REM exemption in both profiles, with
whole-source stability and approval. Specify NEW absence, MV lifecycle, and
valid no-op behavior. C10-C11 state the rule. FILE tests check it. This is a deliberate retained
exception, not an accidental coverage bypass.

### Medium: presentation errors should not weaken target proof

No-op-as-error, duplicate sections, reference churn, and unhelpful stale messages
cause avoidable retries without establishing useful safety properties.

Revision: valid no-op, same-reference section merging, bounded complete-line
repair, preserved old references through verified history, and settled-result
replay. Both profiles receive these improvements. C11/C15-C16.

### Medium: default replacement lacked measurements

Earlier proposals renamed hashline and switched dalgona's default before product
verification or paired edit evaluation.

Revision: keep existing names, aliases, historical calls, output, and defaults.
Add explicit new styles and measurable adoption gates. C01/C18 state the rule. COMPAT tests check it.

## R04. Alternatives considered

| Candidate | Disposition | Reason |
|---|---|---|
| Blind current-coordinate Light | Rejected | Removes stale-target protection unnecessarily. |
| Exact-content or AST-body relocation | Rejected as proof | Equal content may belong to a different occurrence. |
| Fresh whole-file read for every edit | Rejected as default | Discards usable recorded mappings and observations. |
| Per-line digest search database | Rejected | Adds state for the rejected relocation algorithm. |
| New CRDT/distributed editor | Out of scope | Replaces execution architecture instead of improving this tool. |
| Explicit source reference plus recorded edit mapping | Selected | Preserves addressed occurrence under the stated cooperating-writer assumptions. |

The selected design retains the shared engine and Chain. It adds delivery/lifetime
rules and a fixed coverage-policy choice, not a dynamic proof framework.

## R05. Retained requirements versus amendments

| Item | Treatment |
|---|---|
| One patch tool, shared IR, observers, staging and writer | Retained. |
| Before-image coordinates, containment, approval and source caps | Retained and formalized. |
| Seen/Chain | Retained; delivery provenance and revision continuity explicit. |
| Four-hex snapshot header | Replaced only in new profiles by bound references. |
| Explicit file-delete exception | Retained. Both descriptions state it: the Enhanced one at `description.go:13` and the Light one at `description.go:11`. |
| Stale head/tail bypass | Legacy checks the tag before every locator kind, gaps included. Profiles map head and tail gaps through the snapshot store. |
| No-change failure | Profiles return `no_change` for a valid exact no-op. The writer returns `no_change` for an empty batch in every style. |
| Duplicate-file-section failure | Same-reference sections may merge in new profiles only. |
| Output shape | Existing fields retained; versioned metadata added only for new profiles. |
| External drift | Remains a hard history boundary, not auto-relocation input. |
| NEW final-newline behavior | Explicit rule for new profiles; existing-file rules retained. |
| No-clobber NEW/MV publication | Explicit implementation obligation for new profiles; no overwrite fallback. |
| Invocation replay | Proposed runtime/store integration, not a claimed existing feature. |
| Product defaults and aliases | Unchanged. |

The source describes useful Chain mapping while some model-facing prose says to
stop at every stale tag. This package gives the new profiles an explicit contract.
It does not silently rewrite legacy semantics. Its truthful outcome vocabulary
also exposes the source writer's own restore/crash limitations.

## R06. Verification and remaining gates

The included Python model checks 109,673 bounded range/gap cases and eight named
logical regressions. Fresh conversation-runtime and Codexify scratch runs passed
with identical counts. The exported final model was formatted and linted using
Ruff through Codexify. Exact run scope and limitations appear in Conformance V01.

`just hashline-model` runs `verify_model.py` and compares its output with `verification.json`.

Independent reviewer attempts produced no final verdict included in this package.
No reviewer consensus is claimed. Findings above are the lead review supported
by source inspection and the supplied model, not an external approval.

No product Rust compile, actual filesystem/fault/race suite, AST integration,
message-delivery integration, memory benchmark, or live-model evaluation was run.
The bounded model is not proof over all inputs. Production completion requires
those owner-specific conformance gates.

Planning artifacts stay private. No public repository source, defaults, releases,
or pull requests were changed by this task.
