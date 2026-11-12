# Hashline v1: shared specification

Status: implemented in `internal/tool/patch` as the `hashline-light` and `hashline-enhanced` edit styles; neither is a product default.
Date: 2026-09-28.
Applies to: `hashline-light` and `hashline-enhanced`, contract version 1.
Companions: [Light](hashline-light-v1.md), [Enhanced](hashline-enhanced-v1.md),
[Conformance](hashline-conformance-v1.md), [Review and source map](hashline-review.md).

Adopted on 2026-11-08 as the contract for the Go `patch` tool's `hashline-light` and `hashline-enhanced` styles.
MUST and MUST NOT define the contract that these two styles implement. Source-derived requirements and deliberate amendments are
separated in the review. Source locators such as S-PROOF refer to that source map.

## C01. Decision and scope

Use one Hashline parser, one resolver, one staging engine, and one writer. The
only profile-specific acceptance condition is prior observation coverage for an
effective destructive edit. Light omits that condition; Enhanced requires it.
Both retain reference binding, target validation, containment, authorization,
and commit checks.

Keep legacy `hashline`, `anchor`, `replace`, `apply_patch`, their aliases, and
product defaults unchanged. Add the two new styles only as explicit choices.
Never turn an Enhanced refusal into a Light retry. Do not select Light from an
assumed model-quality ranking.

This contract improves the existing `patch` tool. It adds no second tool, second
authoritative store, LSP, CRDT, distributed editor, or dynamic proof-policy API.
The shared-engine architecture is retained from S-ENGINE.

## C02. Guarantee and limits

Every effective existing-file edit MUST identify its source snapshot. Its original
target MUST survive every retained cooperating edit between that snapshot and
preparation. Equal bytes elsewhere MUST NOT establish target identity. Evidence
about the target is separate from permission to change it.

A cooperating edit is an operation whose exact committed mutation coordinates
and before/after digests are supplied by the shared patch engine. A display diff,
filesystem watcher, timestamp, or equal-text search is not authoritative history.

The guarantee is conditional on the history and filesystem observations available
to the host. Arbitrary external writers can race with reads and renames, or modify
and restore bytes between observations. Equal bytes cannot reveal that unobserved
history. Do not claim universal occurrence identity, atomic filesystem snapshots,
or compare-and-swap behavior against non-cooperating processes.

Within recorded history, an edit followed by an undo remains distinguishable from
no edit, even when the final file digest equals the source digest. A known target
conflict MUST NOT disappear because its text later returns.

These rules establish mechanical edit eligibility, not semantic correctness or
user intent. A change elsewhere can invalidate the model's reasoning without
touching its target. Tests and review remain necessary.

## C03. Vocabulary

| Term | Definition |
|---|---|
| Snapshot | Immutable captured file bytes, full digest, and line metadata. Retention is bounded. |
| Document | Host-local logical identity for a file within a workspace. Detected replacement, deletion, or untracked rename breaks continuity. |
| Lineage epoch | Period with a continuous cooperating edit history. A detected history gap ends it. |
| Revision | Monotonic document sequence within an epoch; not a content hash. |
| Read reference | Opaque host-issued handle bound to a snapshot, session-open generation, and consumer. It is not a grant. |
| Consumer | Model context or sequential evaluator receiving tool data. A TUI viewer is not automatically that consumer. |
| Evidence cutoff | Last completed delivery a request is allowed to use. |
| Observed line | Complete unabridged source line delivered to that consumer before the cutoff. |
| Effective footprint | Every existing line cell whose raw bytes the operation changes or removes, including terminator side effects. |
| Gap | Boundary between original line cells or BOF/EOF sentinels. |
| Prepared patch | Private immutable staged plan to which authorization is bound. |

A line cell includes its body and original terminator. The BOM is file metadata,
not line 1. An empty file has zero cells and one gap. Final LF terminates the last
cell; it does not create an extra phantom empty line.

## C04. Public syntax and references

Existing-file example:

```text
[src/lib.rs@r0123456789abcdef0123456789abcdef.1]
PUT 40-42:
+replacement
```

The example reference is illustrative, not usable evidence. A ReadRef is
`r<boot>.<sequence>`: boot is the host's unique 128-bit boot identity rendered as
32 lowercase hex characters; sequence is a nonzero host-wide base-36 counter
using `0-9a-z`, without leading zeroes. The counter MUST NOT wrap or be reused
after eviction. Use the existing host identity facility, not a new randomness
library. A restart uses a fresh boot identity and rejects old-boot references.
Uniqueness relies on that identity facility; the token is not an authentication
secret or an assertion of mathematically impossible collisions.

A lookup MUST also verify authenticated session, session-open generation,
consumer, workspace, and canonical path. Fork, clone, resume, or another consumer
does not inherit executable references. Possession alone grants no authority.

New files use `[PATH#NEW]`. New profiles MUST NOT accept old four-hex `[PATH#TAG]`
as snapshot references. They MUST NOT accept tagless existing-file headers,
`latest`, or inferred references chosen from whichever snapshot happens to work.

| Input | Meaning in the referenced before-image |
|---|---|
| `PUT N:` | Replace source line N. |
| `PUT N-M:` | Replace inclusive source lines N through M. |
| `PUT N.=M:` | Exact alias of `PUT N-M:`; M is an endpoint, not a count. |
| `CUT N`, `CUT N-M`, `CUT N.=M` | Delete the indicated cells. |
| `PUT <N:`, `PUT >N:` | Insert in the source gap before or after N. |
| `PUT >$:` | Insert in the referenced snapshot's EOF gap. |
| `REM` | Explicit file deletion under C10. |
| `MV DEST` | Rename after the file's content edits under C10. |

Every PUT body has at least one `+` row. Strip exactly one leading `+` and retain
all remaining bytes, including indentation. `+` alone is an empty body line.
`+- item` inserts `- item`; `++ item` inserts `+ item`. Zero-row PUT is invalid;
use CUT. NEW permits exactly one `PUT >$:`.

Accept LF/CRLF framing and an omitted final framing line break. Framing omission
MUST NOT alter source newline intent. One outer Markdown fence and one matched
Begin/End Patch envelope may be removed under the source plan's unwrapping rule.
Unpaired envelopes are invalid in these new profiles. Never strip inner fences,
repair body text, or reinterpret another dialect.

Paths are workspace-relative UTF-8 strings. Bare paths are allowed unless they
contain delimiters, leading/trailing whitespace, a control character, or begin
with a double quote. All others use one JSON-quoted string. Header parsing is
quote-aware and expects exactly `@<ReadRef>]` or `#NEW]` outside that string.
In bare paths, `@`, `#`, `]`, CR, and LF are reserved. A leading `[` is legal
because the opening header bracket is consumed separately. `MV "new path.rs"`
uses the same decoder. Decode once; NUL is always invalid.

Grammar-constrained providers and the bounded line parser MUST accept identical
productions. Other providers use the existing JSON envelope `{"input":"..."}`.
There is no Lark runtime dependency or grammar-detection fallback. Legacy grammar
and wire contracts are retained separately (S-GRAMMAR).

## C05. Capture, delivery, and observation

Read/search MUST produce displayed rows and snapshot metadata from the same
immutable captured buffer. Never pair cached rows with a new digest. Capture a
full-file digest and source coordinates even when only part is shown. The capture
is not an atomic OS snapshot against concurrent external writes.

Bind a reference immutably to:

```text
(host boot, session, session-open generation, consumer,
 workspace, document, lineage epoch, revision, canonical path, full digest)
```

Observation coverage is a separate consumer-scoped ledger of completed deliveries.
It can accumulate for the same snapshot. Buffer sharing or digest equality does
not share observation authority.

The reference becomes eligible only after its header has been delivered. Only
complete source rows in that delivery count as observed. Folded blocks, ellipses,
dropped output, truncated rows, and omitted long lines do not count. Escaping may
count only when lossless, unambiguously decodable, and bound by renderer metadata
to the complete source line. BOM and EOL encoding may be metadata rather than
printed characters.

Before sending a model request, freeze its reference eligibility and coverage
cutoff. Every returned tool call uses that same cutoff, including parallel calls.
A preview from one call MUST NOT authorize another already-issued call. Only a
later decision request can use the new delivery. Historical session delivery is
the coverage contract, not proof the model understood or still remembers it.

A sequential evaluator has a distinct consumer identity. A read returned to it
can precede its next nested patch-call cutoff. An internal filesystem lookup by
the patch planner is not such a delivery. Evaluator data does not silently become
model-context evidence. An embedder unable to establish a reliable delivery
boundary MUST reject Enhanced with `observation_unavailable`.

Edit echoes credit only their fully delivered rows. Unchanged cells may retain
coverage through a verified Chain. Replaced cells need a new qualifying delivery.
An evicted/unknown/wrong-boot reference needs a fresh read, not an implicit current
snapshot. Wrong session/consumer/path binding is a rejection and MUST NOT reveal
foreign-context data.

## C06. Revision continuity and authoritative footprints

Check revision and epoch before any digest-equality fast path. Known modification
followed by restoration MUST remain a target conflict. A history gap cannot be
healed merely by restoring equal bytes.

Each Chain edge records source/destination revisions, full digests, and the actual
effective splices committed by the engine. Do not infer preserved occurrences from
a minimal display diff. A broad replacement replaces its entire recorded footprint,
even where the new body repeats old lines.

Other shared-engine dialects may contribute edges only when their exact resolved
mutation is available. An opaque mutation ends the epoch. Shell output or watcher
notifications cannot create an edge. Before following a Chain, verify continuity,
head digest, and current path identity. A missing edge, detected external drift,
uncertain outcome, or inconsistent footprint ends the lineage. Fresh reads may
start a new epoch; old references remain invalid.

Equal-text, equal-node, symbol-name, fuzzy, or nearby-line search MUST NOT establish
edit proof. Diagnostic suggestions are allowed only without automatic retargeting.
This preserves the useful Chain concept of S-PROOF rather than adding a second
content-relocation engine.

## C07. Exact range mapping

Translate source inclusive N-M to zero-based half-open `[a,b)`, with `a=N-1` and
`b=M`. A historical effective splice replaces `[s,e)` with k cells. Exact identity
operations are omitted from history before mapping.

For a nonempty splice (`s < e`):

1. If `max(a,s) < min(b,e)`, return `target_changed`.
2. Otherwise, if `e <= a`, shift both endpoints by `k-(e-s)`.
3. Otherwise, the splice is wholly after the target; retain the endpoints.

For insertion (`s=e`, `k>0`):

1. If `a < s < b`, return `target_changed`; new middle text is not the old target.
2. If `s <= a`, shift both endpoints by k.
3. If `s >= b`, retain both endpoints.

An insertion at range start stays outside and shifts the range. At range end it
stays outside and does not extend it. Never widen a target to absorb new content.

Process disjoint splices from one recorded before-image in descending source
order, then later commits in revision order. First conflict is terminal for that
target/reference; a later undo MUST NOT revive it. Revalidate the final raw
footprint before staging.

A full-file lifecycle guard protects all source content and boundary membership.
A literal `PUT 1-N:` remains a range operation, not an implicit request to delete
an unacknowledged new prefix or suffix. Whole-file operations obey C10.

## C08. Gap mapping

Gap g in n source cells is identified by original left cell `g-1`, original right
cell g, and BOF/EOF sentinels when a neighbor is absent.

For every historical splice:

1. An insertion at g conflicts with `gap_changed`.
2. A nonempty splice replacing/removing either real neighbor conflicts.
3. Other edits strictly before the gap shift it by their line-count delta.
4. Edits after it leave it unchanged.

Both real neighbors MUST still be adjacent. The empty file's sole gap has the
same rule. BOF/EOF belong to the referenced snapshot. A concurrent append occupies
its EOF gap and conflicts. New profiles deliberately remove the source plan's
stale head/tail exception (S-PROOF).

A pure gap removes no line body, so profile coverage does not differ. A terminator
side effect can make it destructive; C11 determines its actual footprint.

## C09. Symbol operations

Symbol locators are outside this contract. A profile MUST reject an operation
whose locator contains `*`, such as `PUT N*:`, `CUT N*`, or `PUT >N*:`. The
error MUST start with `patch: line <n>: block ops (N*) are not supported; use
a line range`, and no target is written. Line ranges and gaps from read or
search output address the same text.

## C10. Create, delete, and rename

NEW requires absence at preparation and commit. Existing symlinks/directories
count as existing paths. It cannot mix with operations on the same destination.
It needs no prior file reference because it removes nothing.

NEW and the MV destination MUST use no-replace publication, not an existence
check followed by an overwriting rename. The writer/store owner must expose
that primitive on each supported platform. If unavailable, reject
`no_replace_unavailable`; do not fall back to an operation that can clobber
a destination created by another process. This requirement does not make
updates to existing files atomic against arbitrary outside writers.

REM requires a valid delivered reference and an unchanged complete source file
and path condition. It cannot combine with other operations on that file. Preserve
the source's explicit-file-delete exemption: neither profile requires every line
to have been displayed for REM. Both descriptions MUST state the exception. Normal
patch authorization still applies; the reference is not deletion permission.

MV requires the referenced complete source, a contained absent destination, and
unchanged source/destination conditions. It may appear once, last under a source
header, after content edits. Its source/destination cannot be independently targeted
elsewhere in the batch. Evaluate edits plus rename from the same before-image.

For REM/MV, unchanged complete source means no effective cooperating content or
lifecycle change since the reference, even if a later undo restores equal bytes.
After MV, invalidate old-path executable references. A destination reference may
carry only correctly transferred consumer coverage. Do not silently follow a
reference to another path.

## C11. Raw bytes and batch composition

Retain source UTF-8, binary/NUL, size, BOM, dominant inserted-EOL, and existing-file
final-newline rules (S-PROOF). Hashline proof never canonicalizes whitespace,
indentation, or quotes. Framing LF is not permission to normalize source bytes.

Expand every effective footprint to include all changed raw line cells. Adding a
separator after an unterminated final line changes that cell. Deleting the final
unterminated line can change its predecessor's terminator. Enhanced needs coverage
of those affected cells too. In both profiles, old mappings through them become
invalid. If no sound line-cell footprint can be recorded, mark the commit opaque;
do not call it a zero-width insertion for future mapping.

For NEW only, join decoded body rows with LF and do not append a framing-derived
LF. One blank `+` creates zero bytes; an explicit trailing empty row adds a final
LF. This settles creation bytes for the new dialects. Existing files retain their
source final-newline behavior.

Every operation under a reference uses source before-payload coordinates. Merge
same-canonical-file sections only with exactly the same reference and consumer.
Different references reject even if digests match. Preserve payload order for
error attribution.

Validate binding, target, path, and authorization-policy eligibility before
eliminating exact identity operations. Never infer no-op from desired-after
text when the reference/target is invalid. A valid no-op removes no cells and
needs no removed-line coverage or human mutation approval. It does not mint a new
revision or prove the task complete.

After no-op elimination, reject overlapping nonempty ranges; adjacent ranges are
valid. Reject same-gap inserts. Within a submitted batch reject an insertion at
or inside a nonempty edit's boundaries: `a <= g <= b`. The caller combines these
edits explicitly. This composition rule deliberately differs from mapping earlier
commits under C07.

Any pre-commit parse, binding, target, coverage, shape, observer, or authorization
failure prevents all target writes. No valid prefix is applied. All-no-op returns
`no_change`; mixed batches apply only effective changes.

## C12. Preparation and authorization

Reserve the full evidence set and staging capacity as one admission unit. Never
hold partial pins or path locks while waiting for missing resources. Acquire path
locks in the existing total canonical order, including rename endpoints.

Only the verified planner can construct private non-Clone `PreparedPatch`. It
contains complete before/after bytes, full digests, actual footprints, path/absence
conditions, profile/version, consumer/cutoff, observer findings, and invocation
fingerprint. The writer accepts that value, not arbitrary spans and a public proof
enum.

Run existing staged observers before asking. They report or block, never rewrite.
Approval binds this exact plan. Release path locks while a person decides; retained
immutable evidence/staging leases remain charged and release on timeout/cancellation.

After approval, reacquire the full write set and recheck digests, revision/epoch,
identity/path conditions, and absences. Any mismatch returns `approval_stale`
without target writes. Never rebase or rebuild under old approval. No-ask policies
still produce an internal authorization decision bound to the prepared plan.
Light never relaxes grants or approval (S-WRITE).

## C13. Commit and truthful outcomes

Reuse the staged writer: create and sync complete temps, dirty affected search
indexes before target mutation, execute ordered target operations, then clean up
and sync under existing POSIX/Windows rules. After the first target operation,
finish commit or rollback before honoring cancellation.

Rollback is an attempt, not an atomic multi-file transaction visible to every
reader. Crashes and non-cooperating writers remain limitations from S-WRITE.
Report observed outcome, not intended outcome:

| Status | Commit state | Meaning |
|---|---|---|
| `applied` | `complete` | Requested effects and required success-record boundary completed. |
| `no_change` | `none` | Valid request with no effective edit; no target writes. |
| `needs_context` | `none` | Evidence missing or insufficient; no target writes. |
| `conflict` | `none` | Known target, gap, path, or approval condition changed. |
| `rejected` | `none` | Invalid request, binding, authority, operation, or resource admission. |
| `cancelled` | `none` | Cancellation completed before any target mutation. |
| `write_error` | `none`, `restored`, `complete`, `partial` | I/O failed; known post-state is stated separately. |
| `indeterminate` | `unknown` | Final effects cannot be established. |

`restored` means writes occurred and were verified restored. Never describe it as
nothing written. `write_error/complete` means effects installed but success/durability
failed, for example a directory-sync failure after renames. `partial` lists every
affected path and verified before/after state. Unknown state requires indeterminate.

Do not publish executable references or Chain edges before commit and its required
record boundary succeed. Invalidate relevant epochs on partial/uncertain outcomes.
A result-journal failure after file writes must not appear as no effect. Stop and
recover under the store contract; do not retry destructively on guessed state.

## C14. Memory and admission

Proposed initial budgets, not measured RSS claims:

| Budget | Host-wide ceiling | Charged allocations |
|---|---:|---|
| Retained evidence | 32 MiB | Snapshots, line indexes, references, coverage, retained Chain metadata |
| In-flight staging | 64 MiB | All prepared before/after buffers and staging-owned metadata |

Share immutable buffers where possible; never share observation authority. Charge
conservatively even when this double-counts shared data. Other runtime allocations
are outside these budgets; total RSS is not promised to fit in 96 MiB.

Retain source ceilings: input 8 MiB; 1-64 edits; 64 canonical paths; 16 MiB per file;
64 MiB staged before-plus-after bytes per invocation. Check before allocation.
These are individual ceilings, not a promise every combination can fit concurrently.
Metadata can reduce usable capacity. Retain at most 256 paths and 16 revisions per
path per consumer, subject to the host-wide bytes.

Evict only unpinned LRU evidence. Issuance counters are independent of retention;
eviction never reuses tokens. Acquire a call's entire evidence/staging lease set
or return `resource_busy`, `resource_limit`, or `reference_expired`. No partial-pin
waits, hidden copies, or unlimited pinning during model think time. An admitted
prepared call pins its evidence until settlement; a printed reference is not a
retention promise.

A requested evidence set exceeding the pool must be split. Never weaken proof to
fit it. If a successful commit cannot retain a fresh echo reference, report success
and a reread instruction without an executable reference. Do not reclassify already
written files as an edit refusal.

## C15. Output and recovery

For the new profiles, retain existing `text`, `error_class`, `changes`, `display`
and add one versioned `hashline` object. Legacy output stays unchanged.

```json
{
  "contract_version": 1,
  "style": "hashline-enhanced",
  "status": "needs_context",
  "commit_state": "none",
  "reason": "unobserved_range",
  "replayed": false,
  "recovery": {
    "kind": "read",
    "path": "src/lib.rs",
    "ranges": [{"first": 40, "last": 45}],
    "mapping": "known",
    "remaining": []
  },
  "path_states": []
}
```

This example is the `hashline` object's value. Status/commit pairs are restricted
to C13. Reason is null for applied/no_change. Recovery is null when no safe repair
is available. `mapping=unknown` MUST NOT present current diagnostic coordinates
as a verified mapping of the original target. Path states use workspace-relative
paths and `before`, `after`, or `unknown` states. `changes` describes verified
actual changes, never the whole proposed plan on partial failure. Display diffs
are not proof.

Recovery respects current read permission and reveals no foreign consumer data.
It may show at most 40 complete lines and 16 KiB of source text total. Never truncate
and register a line as observed. An overlong line yields a read instruction only.
List uncovered ranges in `remaining`. Only the final delivery establishes future
coverage. No internal fetch, automatic retag, or automatic destructive retry.

| Reason codes | Status | Required next action |
|---|---|---|
| `reference_missing`, `reference_expired`, `reference_unknown` | needs_context | Read and copy an eligible new header. |
| `history_gap` | needs_context | Read current content and identify the target again. |
| `unobserved_range` | needs_context | Deliver missing complete rows; submit a new call. |
| `observation_unavailable` | rejected | Use a runtime with a reliable delivery boundary. |
| `cancelled` | cancelled | No target effect; a later request is a new decision. |
| `observer_blocked` | rejected | Address the staged finding; do not bypass the observer. |
| `no_replace_unavailable` | rejected | Use a supported no-clobber writer; no overwrite fallback. |
| `binding_mismatch`, `outside_workspace`, `permission_denied` | rejected | Correct binding/authority; no protected diagnostic leak. |
| `target_changed`, `gap_changed`, `path_changed` | conflict | Inspect current state and decide again. |
| `approval_stale` | conflict | Prepare again; old approval is not reusable. |
| `invalid_syntax`, `invalid_range`, `mixed_references`, `overlap`, `invalid_shape` | rejected | Correct the payload. |
| `resource_busy`, `resource_limit` | rejected | Retry admission or split; never downgrade proof. |
| `invocation_mismatch` | rejected | Do not reuse invocation identity for different input. |
| `write_failed`, `sync_failed`, `rollback_failed` | write_error | Inspect reported effects. |
| `commit_unknown`, `result_record_missing` | indeterminate | Recover and inspect; never blindly re-execute. |

`error_class` maps to the existing vocabulary: applied/no_change/cancelled -> null;
reference/coverage -> proof; history and changed-condition conflicts -> stale;
syntax -> parse; invalid range/mixed references/overlap -> resolve;
path/file shape -> file; permission/observation unavailable -> blocked;
resource -> limit; invocation mismatch -> resolve; I/O/indeterminate -> io.
A staged observer block keeps `blocked`. A parser-support refusal is `resolve`;
no-replace unavailability is `file`. After the first target write, cancellation
does not change an Applied/WriteError/Indeterminate outcome into cancelled.

Diagnostics identify operation index and path when safe, and preserve the current-
dialect reminder for parse errors. A no-op proves neither success of the task nor
that a bug must be elsewhere.

## C16. Invocation identity and replay

Deduplicate before preparation. A bare provider CallId is not sufficient. Runtime
invocation identity binds session, originating open generation, turn/request,
authenticated consumer, and final post-hook payload. Reconnecting clients replay
that identity; a new model call has a new identity even with equal text.

Fingerprint exact final payload bytes, style, contract version, workspace, and
consumer using full digests. Same identity with different fingerprint returns
`invocation_mismatch` before effects. A duplicate still running joins the owner;
cancelling its waiter does not cancel that owner operation.

After bounded syntax/envelope validation and deduplication, append an invocation
start before preparation. Append its terminal outcome whenever the journal stays
healthy, including no_change and pre-commit refusal. Use existing session extension
records, not a new journal. Kinds are `hashline.invocation_started` and
`hashline.invocation_settled`, owned by dispatch/store integration. Start holds
identity, fingerprint, profile/version, and parsed requested paths; settled holds
the terminal output. Records contain no credentials or private-plan citations.

A matching settled record replays without execution, with `replayed=true`.
Historical previews/references from replay create no fresh observation authority.
They may be printed as historical, but the caller needs a new read for fresh
executable evidence. Never mint a current reference while replaying an old result.

After recovery, started-but-unsettled is indeterminate, not safe to retry. An
unstarted old-host/open invocation is not executable either: its evidence expired,
so a new explicit edit is required. Ephemeral sessions deduplicate only within
live-host lifetime. This is recorded-result replay, not a claim of exactly-once
filesystem effects across crashes.

## C17. Implementation boundaries and invariants

Keep existing `ir.rs`, `style.rs`, `resolve.rs`, `write.rs`, `ast.rs`, `tests.rs`
and `styles/hashline.rs`. Add `snapshot.rs` only for one named invariant owner:
reference bindings, deliveries, leases, and revision continuity. Extend a fixed
profile enum; do not add a dynamic ProofPolicy trait or per-line search database.

Internal interface sketch, not executable code:

```text
prepare(profile, request_context, parsed_edits)
    -> PreparedPatch | NoChange | NeedsContext | Conflict | Rejected
commit(prepared_patch, authorization)
    -> Applied | WriteError | Indeterminate
replay(invocation_identity, fingerprint)
    -> RecordedResult | RunningSubscription | New | Indeterminate | Mismatch
```

PreparedPatch construction is private. Leases release exactly once. The writer
cannot create observation credit or alter the profile. NeedsContext cannot be
converted into PreparedPatch without a subsequent eligible decision.

| Invariant | Required property |
|---|---|
| INV-REFERENCE | Effective existing-file edits have eligible bound source references. |
| INV-LINEAGE | No target crosses a missing or invalidated history edge. |
| INV-OCCURRENCE | Later equality never revives a known-invalidated occurrence. |
| INV-COVERAGE | Enhanced affects no unobserved existing cell, except explicit REM. |
| INV-CUTOFF | Future deliveries never authorize pending calls. |
| INV-AUTHORITY | Neither references nor Light grant mutation permission. |
| INV-PREPARED | Commit effects are exactly the authorized immutable plan. |
| INV-NOWRITE | Pre-commit refusal has no target effects. |
| INV-OUTCOME | Reported effects never exceed what is known. |
| INV-REPLAY | Settled invocations are not executed again by replay. |
| INV-BOUNDS | Accounted leases, buffers, and metadata stay within their budgets. |

## C18. Adoption

Preserve `strict -> hashline`, `balanced -> anchor`, `simple -> replace` and legacy
call semantics. New style changes affect subsequent model requests only. Each
historical call retains its recorded style/version.

Do not change defaults on the strength of this proposal or the bounded model.
Require conformance, legacy regression tests, platform faults/races, host resource
measurements, and paired end-to-end edit evaluations. Measure cost per verified
edit and wrong-target writes. Separate no_change, replay, genuine conflicts, and
newly applied edits. Reducing a reported error count is not permission to conceal
incorrect writes.
