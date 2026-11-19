#### Specifications step 39a. Correct `docs/decisions/codex-protocol-as-core.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/decisions/codex-protocol-as-core.md`, and the sentence `The merge MUST keep order.` in `docs/spec/protocol.md` line 22, in one commit. Line numbers are from commit `3c4b6f94`.

Line 7 says: "It MUST preserve request identifiers and notification order." The code is right. `docs/spec/protocol.md` line 22 is wrong, because it says `The merge MUST keep order.` and the code does not keep that order. The sentence is too strong. Under backpressure the writer merges a text delta into the newest queued delta of the same method and item, even when other messages were queued after that delta.

Replace this statement. Keep the rest of line 7 as it is.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 7 | It MUST preserve request identifiers and notification order. | It MUST preserve request identifiers and notification order. Under backpressure, text deltas MAY merge as [protocol.md](../spec/protocol.md) defines. A merged delta MAY then reach the client before a message that was queued between the two deltas. | `docs/spec/protocol.md:22` says: "Delta notifications for `item/agentMessage/delta`, `item/reasoning/textDelta`, and `item/commandExecution/outputDelta` MAY merge into the newest queued delta for the same item." The merge runs in `internal/rpc/conn.go:566-576` `merge`. Line 567 returns false when `len(q.items) < 1024` or the item has no stream, so a merge needs at least 1024 queued messages and a delta. Lines 570-575 look up `q.last` for the method and item, append the new text to `prev.text`, and return true. `q.last` holds the newest queued delta for each method and item (`conn.go:542-543`), so a merge reaches back past any messages queued after that delta. `conn.go:536-540` `finish` returns early when the merge succeeds. `conn.go:620-623` `isDeltaMethod` lists the three delta methods, and `conn.go:553-555` `newQueued` uses it. |

The new text keeps the rule for request identifiers and for notification order. It adds one exception in a separate sentence that points to `docs/spec/protocol.md`. The exception needs no other change in the decision file.

Proof: line 7 matches the Write column, the link target `docs/spec/protocol.md` exists, and `docs/spec/protocol.md` line 22 replaces `The merge MUST keep order.` with `A merge keeps the text of one item in order. It MAY place merged text before messages queued between its two deltas.`

