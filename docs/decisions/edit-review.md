# Edit review

## Invariant

The edit review MUST stay off unless `[edit_review] enabled` is true. The judge MUST NOT approve or reject an edit. A judge failure MUST NOT block an edit. Recovery MUST NOT judge. Raw secrets MUST NOT reach the judge. Each request MUST encode to at most 30000 bytes. The `sound` question MUST only gate speech. Each question MUST score on its own. At most one turn review MUST run per turn id.

## Scope

The edit unit and its judge goroutine live in `internal/agent/review.go`. The judge is the decider named by `[approval_decider]`. The decider provider receives the masked content of each reviewed edit. The approval decider receives only action fields, so the edit review is the first path that sends file content to the decider. Masking is best effort.

## Enforcement

`internal/agent/review_test.go` tests the verdict, the failure modes, the packing, the masking, and the switch. `TestRecoveryDoesNotJudgeReapprovedEdit` and `TestRecoveryNeverJudgesCompletedEdit` hold recovery to no judge call. The 30000-byte review limit is enforced through `reviewRequestLimit` in `internal/agent/review.go` and `internal/agent/review_diff.go`, with assertions in `internal/agent/review_test.go` (lines 516-521). `internal/decide` tests `RequestSize` accounting against the posted body.

## Rejected alternatives

- A single judge request for a large edit. A request above about 32,000 bytes runs in the less accurate long-context mode, so the unit splits into parts.
- Judging each edit before the next tool runs. The edit would then wait up to 8 seconds per call.
- Writing the verdict inside the `tool.completed` output. The journal is append-only, and the verdict does not exist when `tool.completed` is written.
