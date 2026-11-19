#### Specifications step 4. Correct `docs/spec/approvals.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/approvals.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 3 | Human prompts occur only in approval mode `ask`. | User prompts occur only in approval mode `ask`. | internal/agent/gate.go:251 `func (w *worker) human(` is the path that prompts the user; `by` value `user` (approvals.md:11) |
| 5 | A per-turn approval policy MUST NOT lower the thread mode: the effective mode is the stricter of the thread mode and the turn mode. A thread in `ask` therefore keeps human prompts when a client sends `on-request` for a turn. | A per-turn approval mode MUST NOT lower the thread mode: the effective mode is the stricter of the thread mode and the turn mode. A thread in `ask` therefore keeps user prompts when a client sends `on-request` for a turn. | internal/app/cli.go:173-176 `func approvalPolicy(mode string) string { switch mode { case "yolo": return "never"`: each mode maps to one wire policy, so the two words name one value; `on-request` stays as the wire value |
| 30 | 6. Human, mode `ask`. | 6. User, mode `ask`. | docs/spec/approvals.md:30 step 6 heading; internal/agent/gate.go:251 (`human`) is the code path for step 6 |
| 32 | Child threads change step 6. | Subagent threads change step 6. | internal/agent/gate.go:297-298 `parent, open := t.host.loadedThread(t.created.Parent)` / `if !open \|\| t.created.Advisor != ""` (the parent-linked, non-advisor thread is the subagent) |
| 32 | Only parent subscribers count as clients for such a child. | Only parent subscribers count as clients for a subagent. | internal/rpc/serve_child_approval_test.go:116-140 (no parent connection declines) |
| 32 | When the request resolves, the child MUST publish the same `UpdateApproval` with `Resolved=true` to the parent's subscribers. | When the request resolves, the subagent MUST publish the same `UpdateApproval` with `Resolved=true` to the parent's subscribers. | internal/rpc/serve_child_approval_test.go:95 `expect(t, fieldOf(t, resolved.Params, "threadId") == childID, "serverRequest/resolved thread", ...)` |
| 32 | The app-server MUST then send `serverRequest/resolved` with the child `threadId` and the `requestId`. | The app-server MUST then send `serverRequest/resolved` with the subagent `threadId` and the `requestId`. | internal/rpc/serve_approval.go:113 `s.send("serverRequest/resolved", ServerRequestResolvedNotification{ThreadID: threadID, RequestID: requestID})` |
| 32 | When the last parent connection leaves, the app-server MUST decline the pending child request. | When the last parent connection leaves, the app-server MUST decline the pending subagent request. | internal/rpc/serve_child_approval_test.go:102-113 `TestServeChildApprovalDeclinedWhenParentConnectionLeaves` expects decision `decline` |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 30 | An advisor thread MUST NOT wait for a user decision. In mode `ask`, it MUST use step 5 instead of step 6. | internal/agent/gate.go:252-253 `if w.t.created.Advisor != "" { return w.automated(ctx, gc) }`; internal/agent/gate.go:186-203 `automated` calls only `decider.Decide` |
| 30 | A `-p` turn answers every approval request with `decline` and prints a notice to stderr. | internal/app/cli.go:734-744 `turnBook.Request` returns `rpc.CommandExecutionRequestApprovalResponse{Decision: "decline"}` and writes `declined ...: no interactive client` to stderr; internal/app/cli.go:454 `book *turnBoo |
| 43 | An allowed verdict MUST use the same reason form, with `allowed` in place of `declined`. | internal/agent/decider.go:113-116 `verb = "allowed"` and `reason := fmt.Sprintf("%s by decider %s: %s (threshold %s)", verb, ...)`; internal/agent/decider.go:133-139 `scoreText` prints `reversible=` and the optional `ser |

Use one word for one thing.

- Keep `approval mode`. Replace approval policy at lines 5.
- Keep `user`. Replace human at lines 3, 5, 30.
- Keep `subagent`. Replace child at lines 32.

Structure:

- Line 32: the advisor exception (`other than an advisor`) is dropped from the sentence. An advisor is not a subagent, and the step 6 advisor sentence added after line 30 covers it.
- Resolved, confirmed, no change (line 30, exact command text): internal/rpc/serve_approval.go:44 `Command: cmdText(prompt)` and :98 `return cmp.Or(prompt.Command, ...)` send the exact text. The client rendering is outside this file.
- Resolved, confirmed, no change (line 32, child approval routing): internal/rpc/serve_child_approval_test.go:82-140 (resolved to parent connection, decline when parent leaves, headless decline); internal/rpc/serve_approval_dedupe_test.go:51 `TestServeChildApprovalSentOncePerConnection`.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 19 | Counting stops at the most recent `user.message`. Counting also stops at the most recent `tool.started` record with a different key when that record is class `edit` or `exec` and completed with `ok=true`. |
| 25 | A hook failure MUST be logged. A hook failure MUST count as no opinion for that hook only. Later hooks MUST still run. |
| 28 | For `remote` calls, only the semantic deny rules apply. They check each decoded string value of the JSON arguments. Shell-syntax rules never see the raw JSON. |
| 30 | For an `exec` call, the prompt carries the exact command text. The app-server MUST show that text verbatim, not as a reassembled form. |
| 32 | A subagent MUST also publish its `UpdateApproval` to the parent's subscribers, with `Update.Thread` set to the subagent id. |
| 32 | A connection that watches both threads MUST send each approval request id once. A repeated delivery of the same request id MUST NOT send a second request. |
| 49 | When the resolution is journaled before the class is known, the fold carries the grant prefix forward. The fold creates the grant from the later `tool.started` record. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 10 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/approvals.md` prints `style findings: 0`, and `just verify` passes.

