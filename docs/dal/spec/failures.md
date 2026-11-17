# Failure records

The failure package owns bounded masked records of product failures. It has no network imports. `dal` links it directly. Records and capture MUST land atomically with correlation, bounds, and refusal behavior. No landed revision may expose records without those contracts.

## Classification

The closed `Class` values are `panic`, `tool`, `model`, `journal`, and `runtime`. A person decline, cancellation, interrupted turn, skipped call, or permission refusal is not a failure. Expected invalid tool input, missing requested files, and edit conflicts are ordinary tool results. An ordinary nonzero command exit is not a product failure unless the exec tool itself failed. Classification uses typed provenance, never message matching.

Local capture includes side-conversation failures. Remote reporting excludes them. One terminal model failure is captured after retries and failover finish.

## Record shape

```go
type Class string
type Origin string

type Frame struct {
    Function string
    File     string
    Line     int
}

type Record struct {
    SchemaVersion int
    ID            string
    Time          time.Time
    Product       string
    Version       string
    OS            string
    Arch          string
    Class         Class
    Tool          string
    Error         string
    Stack         []Frame
    Args          string
    Output        string
    Fingerprint   string
    ContentFree   bool
    Reportable    bool
    FailingStep   *FailureStep
}
```

`Build` takes an `Input` with id, time, product, version, class, tool, error, frames, args, output, workspace, origin, correlation, and failing step, plus a `Masker`. Production uses the auth store. Tests use an in-memory or failing masker.

Record JSON members are exactly `schemaVersion`, `id`, `time`, `product`, `version`, `os`, `arch`, `class`, `tool`, `error`, `stack`, `args`, `output`, `fingerprint`, `contentFree`, `reportable`, and `failingStep`. Frame members are `function`, `file`, and `line`. Schema version is 1. Time is UTC RFC3339Nano. Correlation is stored separately and has no public Record member.

Ids are 16 cryptographically random bytes encoded as 32 lowercase hex digits. An empty id generates one. An empty version becomes `unknown`. Empty tool and excerpts are valid. Product is exactly `dal` or `dalgona`. Unknown product, class, or origin MUST fail with `failure: invalid record`. Nonempty tool names MUST match `[A-Za-z0-9_.-]{1,64}`. Versions MUST match `[A-Za-z0-9_.+-]{1,80}`. Invalid values become `unknown`. Origins are `main`, `side`, `doctor`, and `test`.

## Masking and bounds

`Build` MUST decode valid argument JSON and mask every string key and value. Invalid JSON is treated as plain text. All text runs through the masker, then the built-in secret scanner regardless of configuration. Residual matches become `[redacted]`, longest first with deterministic lexical ties.

The public privacy policy applies before truncation to every serialized string. Workspace paths become workspace-relative with `/`. Rejected escapes become `[path]`. URL userinfo, query, and fragment are removed. Prompts, session turns, correlation keys, file contents, environment values, usernames from paths, and the project repository name MUST never appear. File hashes can reveal whether guessed content matches, so their privacy implication MUST be documented to the person enabling reporting.

Post-mask UTF-8 limits are 4096 bytes for error and output, 2048 for arguments, and 32 frames. Function and frame file names are limited to 256 bytes each. `[truncated]` counts inside each limit. Input over 1 MiB raw per field selects content-free fallback without unbounded processing.

Masking failure or no masker MUST discard every detail. The fallback sets `ContentFree` true with error `failure details withheld: masking failed`. It keeps empty arguments and output with no frames. Its `FailingStep` carries only schema version 1, `unavailable` status, and reason `failure case unavailable: masking failed`. It retains only validated metadata, id, time, and class. It MUST NOT include the masking error.

Frames normalize by removing headers, arguments, offsets, receiver addresses, and compiler generic shape arguments. Runtime and failure-package frames drop out. Order is preserved with the first 16 remaining frames. Module files are relative to `github.com/gosuda/dalgona`. Standard library files use `std/<package>/<base>`. Other files use `[external]/<base>`.

## Fingerprint

`Fingerprint` is the full lowercase SHA-256 over NUL-separated fields `failure-v1`, product, class, tool, then each normalized `function@file`. No trailing NUL is used. Product version and line numbers never enter the hash. A version-only rebuild creates no new issue. Empty and content-free stacks contribute `no-stack`. Ids, error text, arguments, output, and timestamps never enter it. SHA-256 collision risk is negligible, but semantic coalescing is not: distinct stackless failures of one product, class, and tool share a fingerprint. Fingerprint equality MUST NOT prove that a local case reproduces an issue.

`Reportable` is true only for dalgona origin main. `ReportText` renders the shared title and body from masked records with no network imports. `dal` consumes it directly for manual output.

## Converter

The converter belongs to the package that owns the record. Conversion needs no battery imports and is available in `dal`. No second converter or second failure package may exist. The typed contracts are `Correlation`, `FileRequirement`, `RecordedCall`, `ScriptToolCall`, `ScriptStep`, `FailureStep`, and `LocalRecord`, with `BuildFailureStep`, `ValidateFailureStep`, and a local store reader.

Correlation holds session, turn, and call id. It is local-only, stored in a 0600 sidecar under the 0700 failure directory, at most 4096 bytes. Public call ids remap to the synthetic constant `failure-call`. Session and turn ids and timestamps MUST never enter attachments. A missing sidecar MUST fail with `failure case unavailable: local correlation is missing`.

The public attachment is an allowlisted failing step only. It holds one recorded call plus the script step that re-emits that same call. It holds one finish step with tool calls and at most 32 file requirements. Arguments are masked, valid object JSON, at most 8192 encoded bytes. File paths are at most 256 bytes and the attachment JSON is at most 16384 bytes. No file contents, earlier prompts, session events, output blobs, raw errors, or original correlation ids enter it. Inline file-content payloads select `unavailable` rather than publish content.

Every automatic issue contains an attachment envelope, including unavailable cases. Status is exactly `ready` or `unavailable`. Unavailable envelopes carry only schema version, status, and reason. Over-bound arguments yield `unavailable`, never partial executable JSON. No automatic conversion runs commands or spends model tokens.

| Condition | Exact reason |
|---|---|
| No local correlation | `failure case unavailable: local correlation is missing` |
| No recorded call or completion | `failure case unavailable: recorded tool completion is missing` |
| Freeform call | `failure case unavailable: freeform tool calls are not supported` |
| Non-object arguments | `failure case unavailable: tool arguments are not an object` |
| Provider failure without tool completion | `failure case unavailable: provider failures cannot be replayed` |
| Panic without a completed call | `failure case unavailable: panic has no completed tool call` |
| Placeholder-dependent replay | `failure case unavailable: masked arguments cannot be replayed exactly` |
| Secret masking failure | `failure case unavailable: masking failed` |
| Masked pre-image mismatch | `failure case unavailable: masked pre-image cannot seed the recorded state` |
| Required timing or scheduling | `failure case unavailable: timing and concurrency are not recorded` |
| Required sandbox or process state | `failure case unavailable: sandbox and process state are not recorded` |
| Required network or remote service | `failure case unavailable: network and remote state are not recorded` |
| Required environment or filesystem state | `failure case unavailable: environment and external state are not recorded` |
| Signature or redacted thinking | `failure case unavailable: provider-specific call state is not supported` |
| Inline file-content payload | `failure case unavailable: inline file content is not included in cases` |
| Bound exceeded | `failure case unavailable: replay data exceeds its limit` |
