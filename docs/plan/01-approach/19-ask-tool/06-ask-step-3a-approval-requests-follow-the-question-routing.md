#### Ask step 3a. Approval requests follow the question routing rule

Files:

- `internal/rpc/serve_approval.go`: in `(*server).request` (`:116`), the `default:` branch (`:131`) replaces `s.answer(a, agent.ReplyDecline)` (`:133`) with `s.declineIfAlone(a)`. `declineIfAlone` (`:137`) declines only when `Status().Subscribers <= 1`, the rule that questions get from `declineQuestionIfAlone` in Step 3. With two windows attached, the approval stays open for the other window. The encode branch (`:120`) and the `serveCtx` and closed-pipe branch (`:128`) keep their behaviour.
- The first answer already wins through `Thread.Answer` (`serve_approval.go:145-150`), which ignores `ErrUnknownRequest`. The other window already gets `serverRequest/resolved` through `s.resolved` (`:111`). Nothing changes there.

Tests in `internal/rpc/serve_approval_test.go` (new file), package `rpc`. They use `newRig(t, config.ApprovalAsk)` (`serve_harness_test.go:138`) and `r.connect()` (`serve_harness_test.go:216`). A client sends its error reply through `wireClient.send` (`serve_harness_test.go:334`).

- `TestApprovalErrorWaitsForOtherWindow`: two clients subscribe to the thread. A command approval goes out, and the first client replies with a `-32603` error. The request stays pending, and the second client's `accept` runs the command. It fails before this step, because the request is declined at once.
- `TestApprovalErrorDeclinesWhenAlone`: one client, the same error. The command does not run. It passes before and after and guards the alone case.

Docs: `docs/spec/approvals.md`: one paragraph in `## Gate order`, after the child-thread paragraph and before `Writable roots are`. It says that an error from a client means "cannot answer". It declines only when that window is the thread's only subscriber. Otherwise it leaves the request open. The first answer wins. The protocol workstream, Step 13 adds its own Accept sentence to the same section, and this step does not edit it.

- Lands after: Step 3 (the question routing rule that this step applies to approvals).

Anchor: Herbert Simon.

