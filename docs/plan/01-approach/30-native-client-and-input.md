# Native client and shared Unicode input

## Status and authority

This workstream defines implementation work for the complete Go and Gio desktop client.
It also aligns input behavior across GUI, TUI, web, and daemon.
It records no completed runtime, desktop, IME, or platform acceptance result.

The [native overview](../../dalgona/spec/native.md), [architecture decision](../../dalgona/decisions/native-client.md), and [input contract](../../dal/spec/input-text.md) own the requirements.
The inspected source baseline is `2c036c60b379e7718255a23ebd2c5079d76ec50c`.
Read current source before implementation. Older plan filenames and line references do not override these contracts.

Gio owns native layout, input, and rendering. The daemon retains execution, policy, credentials, and journal authority.
The application must contain no Electron runtime, WebView, browser DOM, or HTML layout engine.
Explicit OAuth authorization may open the system browser.
Follow the existing web design, visual tokens, compact panels, focus rules, and single primary action.

## Work order and completion

These work packages are dependency order for one complete release.
They are not permission to ship missing components or replace a required flow with an unsupported message.

|Order|Implementation work|Exit evidence|
|---|---|---|
|1|Extract shared Go connection ownership and separate client wire code from server handlers.|Existing TUI, web relay, app-server, and command paths pass their lifecycle and boundary checks.|
|2|Implement strict text ingress, source-preserving drafts, segmentation, and typed question correction across existing clients.|The shared vectors pass through production adapters. Invalid requests have no partial effects.|
|3|Implement native capability negotiation, ordered delivery, bounded history, and decision details.|Snapshot, backpressure, reconnect, and request-identity fixtures pass.|
|4|Build the Gio shell and every mapped web component.|All C01-C43 workflows complete, including loading, empty, failure, cancellation, and recovery states.|
|5|Complete native document selection, rich rendering, figures, math, images, and exports.|Source copy, virtualized navigation, bounded workers, and renderer fixtures pass.|
|6|Complete authentication, remote management, MCP, workspace files, and real terminals.|All C44-C51 workflows complete against their authoritative endpoint and runner.|
|7|Complete native input, clipboard, dialogs, menus, accessibility, and font behavior.|Every declared OS, architecture, display backend, CJK IME, and screen-reader gate has evidence.|
|8|Package the application and verify installation, update, resource bounds, and performance.|The complete native verification matrix passes with reproducible results.|

The implementation must retain headless binaries and remote box builds without graphics dependencies.
Dependency changes include exact pins, licenses, assets, and the existing dependency decision.
No runtime package is complete merely because this plan names it.

## Shared Unicode input

Use [Input text](../../dal/spec/input-text.md) and its [fixture corpus](../../dal/spec/input-text-cases.json) in every adapter.
Pin Unicode 18.0.0 default extended grapheme rules and the upstream conformance artifacts.
Verify the pinned implementation rather than assuming a browser, Go library, or Gio version supplies those rules.

- Preserve accepted scalar sequences, including unusual symbols, decomposed CJK, private-use characters, variation selectors, and emoji sequences.
- Keep display escaping, search keys, and font fallback separate from canonical input and authoritative identities.
- Replace TUI private-use paste markers with tagged metadata outside Unicode text.
- Keep bytes, scalars, UTF-16 units, grapheme boundaries, terminal cells, and shaped geometry distinct.
- Make paste, completion, composition commit, and selected replacement atomic undoable edits.
- Keep IME ownership through the final committing key event. Synthetic events cannot replace actual candidate-window testing.
- Apply CRLF/CR conversion once to newly authored multiline drafts. Reject multiline insertion into single-line fields without suffix loss.
- Preserve offered option values unchanged, including their original line endings.
- Enforce the answer limit of 2000 scalars without trimming or truncation.
- Reject blank required model question fields before offering a question. This eligibility check must not rewrite accepted values.
- Determine conversation-search eligibility from each Unicode-whitespace-separated term. Every term needs at least three scalars.
- Send eligible search text unchanged. Padding must not make a short term eligible.
- Validate before discovery, trust changes, credential updates, file reads, blobs, or journal effects.
- Preserve secret masking and existing raw file, PTY, and journal byte contracts.

Question correction needs both server and client changes.
The daemon validates a structurally valid answer before consuming its waiter or claiming the first valid answer.
The client keeps its response sender, card, and draft until `serverRequest/resolved`.
A matching correction warning permits a corrected response with the same request ID and original deadline.
Timeout, disconnect, and last-subscriber rules remain authoritative.

## Snapshot and event ordering

Implement the [R04/R05 barrier](../../dalgona/spec/native-protocol.md#r04-one-ordered-delivery-stream) in the connection dispatcher.
Finish pending coalescing before choosing a snapshot cut, then freeze the projection before later events change it.
No queued or emitted envelope may straddle the cut.
The client preserves envelope boundaries while a cut is pending and validates global ordinal continuity before filtering by thread.
Discard only envelopes wholly covered by that thread's snapshot. Apply only envelopes wholly after it.
A straddling envelope requires resynchronization without guessing a suffix.

Test an initial open and a replacement while several same-item deltas are pending.
Also test concurrent snapshots for two threads and a delayed open response with client-side buffering.
Every source byte must appear once after reconciliation.

## Authentication and MCP

Implement the exhaustive flow variants in [R09](../../dalgona/spec/native-protocol.md#r09-provider-mcp-and-reporter-authentication).
API-key and Cursor deep-link starts omit `callbackMode`.
Other flows require their declared callback mode. Reject mismatches before starting authentication.
Direct key login creates no browser or listener and accepts the exact key through its returned transaction.
Remote callback reachability restricts callback-based OAuth only. Offered callback-free flows remain available without a device or manual fallback.

Implement [R10](../../dalgona/spec/native-protocol.md#r10-mcp-configuration-and-runtime-status) provenance before joining configuration and runtime status.
Retain source identity through discovery, configuration precedence, and runtime registration.
Negotiated status requests and pages bind the selected thread and inventory version.
Only an effective inventory row with the same name and source can receive that runtime state.
Reject missing provenance as incompatible and changed inventory versions as stale.
An absent runtime row means not started. A shadowed row must not inherit another source's status.

Test callback-free key login, illegal mode combinations, duplicate MCP names across sources, and configuration changes between status pages.
Do not infer provenance from a server name or URL.

## Workspace ownership and release

Implement explicit [workspace close](../../dalgona/spec/native-protocol.md#r11-runner-aware-workspace-authority).
The shared client owns local interests from windows, files, completions, commands, and terminal attachments.
Closing one view releases only that interest.
The final interest releases the server handle after dependent operations and attachments finish.

A close with a terminal attachment or dispatched mutation returns `busy` without cleanup side effects.
A successful close cancels pending reads and searches, removes watches and cursors, aborts uncommitted uploads, and frees its slot.
It must not delete files, stop a terminal, or revoke another view's grant.
Test more than eight sequential open/close cycles, shared views, active operations, and final-reference cleanup.

## Terminal replay and final output

Implement [R12](../../dalgona/spec/native-protocol.md#r12-real-terminal-sessions) with one authoritative VT processor.
Each committed resize has a terminal-wide ordinal and output-byte offset.
Every attachment receives ordered resize records, including read-only attachments.
Checkpoint state binds both cursors. Reads resume with the byte cursor and last applied resize ordinal.
Resize acknowledgments must not independently change the replica's grid ahead of unread output.

Test multiple resizes at one byte offset, bounded pages with zero bytes, missing resize records, and incomplete UTF-8 or terminal sequences.
Byte or resize eviction requires checkpoint recovery. It must not silently skip geometry changes.
Refresh an existing reader through `dalgona/terminal/checkpoint/open` without allocating or detaching an attachment.
Publish replacement checkpoints atomically within the shared budget. A refused refresh preserves the old checkpoint and attachment.
Test recovery with four occupied slots and with the only reader of a finalized terminal.

Stop revokes writing but preserves read attachments through bounded output draining.
Final status identifies the final byte and resize cursors, truncation, and a fixed retention deadline.
Read until both final cursors are reached before declaring output collected.
Explicit detach releases that attachment. The final detached reader can release finalized retained state early.
Readers cannot extend the fixed sixty-second retention by polling or reconnecting.
Retention expiry ends collection, releases local attachment and workspace interests, and marks any missing tail beside collected output.

Test final output emitted during shutdown, drain timeout, repeated close, reader detach, and retention expiry.
Detach terminal attachments before releasing their workspace reference.
Uncertain write or resize outcomes must not cause automatic input replay.

## Acceptance and reporting

Use [native verification](../../dalgona/spec/native-verification.md) for the complete behavioral and platform matrix.
Update the affected existing plan fragments when implementing these requirements.
Do not reintroduce their replaced cleanup, quota, callback, or resource-lifecycle rules.

Run `just verify` before each implementation commit and add the required dedicated desktop gates.
Report local checks, manual native checks, live-service checks, and unavailable evidence separately.
Do not count fixture syntax validation, screenshots alone, or source-text assertions as runtime behavior proof.
Documentation changes require style, links, review, and the repository's mandatory verification attempt.
