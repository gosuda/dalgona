# Native client verification

## Evidence and completion

This document defines required implementation evidence for the [native client](native.md).
It records no completed runtime test.
The documentation PR does not satisfy these runtime release gates.

T01. A complete release MUST pass every applicable contract in this suite.
An implementation slice MAY land separately, but MUST NOT be described as the complete native release.
Required diagram, terminal, authentication, accessibility, and platform work MUST NOT be deferred behind a completion claim.

Each release evidence record MUST identify the commit, dependency pins, OS, architecture, display backend, GPU, driver, locale, and input method.
It MUST identify the exact command or manual procedure, expected result, actual result, and evidence artifact.
A skipped test MUST remain a skipped test.
The record MUST distinguish automated checks, inspection, manual native checks, and live-service checks.

T02. Tests MUST observe behavior through the relevant boundary.
Their expected results MUST come from an independent oracle or an explicit contract.
Source-text assertions, screenshots alone, and a successful build MUST NOT prove interaction behavior.
The implementation MUST follow [CONTRIBUTE.md](../../../CONTRIBUTE.md) for mutation probes and [REVIEW.md](../../../REVIEW.md) for review evidence.

## Coverage map

The following map defines the minimum evidence for each group.
Every component in [native-components.md](native-components.md) MUST appear in the implementation's test inventory.

|Contract group|Required evidence|
|---|---|
|N, complete product|All required workflows have a tested route from opening the application to success or an actionable refusal.|
|A, architecture|Race tests, lifecycle faults, bounded queues, reconnect, stale results, and import checks.|
|R, protocol|Schema validation, server and client conformance, old-client compatibility, resource limits, and authorization failures.|
|V, design|Token conformance, measured geometry, keyboard behavior, font fallback, and native visual inspection.|
|C01-C43, web component mapping|Behavior tests for each mapped component and its loading, empty, failure, cancellation, and success states.|
|C44-C51, native additions|Thread management, endpoints, runners, MCP, files, terminal, diagnostics, and consented reporting.|
|D, documents|Selection, copy, layout, renderer limits, diagram and math fixtures, image bounds, and terminal behavior.|
|P, platforms|OS dialogs, menus, notifications, IME, accessibility, installation, updates, and uninstall.|
|I, shared input text|The shared Unicode corpus, actual editor transactions, strict ingress validation, and real CJK composition on every claimed client.|

## Deterministic protocol and lifecycle fixtures

T03. A scripted local daemon MUST drive the following cases without provider credentials.
The fixture server MUST use the production framing and validated schemas.
Fault injection MUST act at the stream or operation boundary, not bypass production state handling.

|Case|Required observation|
|---|---|
|Split a frame across reads|The client receives one complete decoded message.|
|Send the largest permitted native payload|The message fits its negotiated method and queue limits without a redundant whole-payload copy in the display path.|
|Exceed the wire limit|The connection fails with a bounded diagnostic and no partial mutation.|
|Reuse an item ID in another turn|Both rows remain distinct, and each delta reaches its own row.|
|Reuse a thread ID on another endpoint|Each lane keeps a distinct thread. Drafts, credentials, history, requests, and paths remain separated.|
|Reuse an operation ID on another endpoint|Each receipt remains keyed by its EndpointID. Reconciliation reads only the owning endpoint's receipt.|
|Stream turns on two endpoints, then lose one|Its projections become read-only. After its retry ladder it is Offline. The other endpoint continues streaming and sending.|
|Connect a ninth endpoint with an eligible connected session|The least-recently-used session without attachments or calls in flight enters Idle. Live transports never exceed eight.|
|Connect a ninth endpoint when every session has attachments|Admission reports capacity without closing a session or exceeding eight live transports.|
|Connect a daemon remote through its relay|TLS 1.3 and the Authorization bearer header authenticate the native client. Without the header, the browser query token works. No token enters a URL or log.|
|Complete a turn before a delayed start notification|The completed status and its error remain intact.|
|Deliver an older status sequence|The current thread status does not regress.|
|Reconnect with a new connection generation|Late responses cannot change the current state. Calls on a non-current link fail before dispatch.|
|Append output during snapshot creation|Every item and byte appears once after snapshot and live-state reconciliation.|
|Open or replace history while same-item deltas await coalescing|The dispatcher seals the envelope before the cut. No envelope straddles that cut.|
|Buffer deltas while two thread snapshots are pending|The client preserves envelope boundaries and filters each thread by its own cut after validating global continuity.|
|Receive an invalid envelope spanning a snapshot cut|The client resynchronizes without applying a guessed suffix or duplicating snapshot text.|
|Resume or fork a large conversation|Bootstrap responses and notifications remain bounded. Existing content arrives only through paged history.|
|Complete a message larger than the mailbox|Live delivery carries bounded headers and immutable content references without hydrating the full message or reconnecting repeatedly.|
|Split one turn across history pages|Merging pages preserves every item and distinguishes partial from complete turns.|
|Expire a history token during paging|The client reports expiry and reopens an authoritative snapshot without mixing page generations.|
|Lose the stream after a mutation write|The client displays an uncertain outcome and does not repeat the mutation automatically.|
|Fill the event queue|Memory stays within its bound, and critical events are preserved or the connection is explicitly resynchronized.|
|Minimize the window during a burst|The reducer continues bounded state handling without requiring a visible frame.|
|Open another thread during a slow search|The previous result does not replace the new thread's completion or search list. Results from other endpoints remain distinct.|
|Detach the active thread|Only its owning endpoint connection ends that subscription. Other endpoints and clients keep receiving updates.|
|Close one of two windows viewing a thread|The other window retains its subscription, pending decisions, and live updates.|
|Open a ninth actively referenced thread|The application reports its subscription limit without detaching another window's thread.|
|Refresh history with eight active snapshots|Atomic replacement uses the existing slot. Failure preserves the old snapshot and an export's retained handle.|
|Close while a worker is blocked|Cancellation completes, the child is reaped, and no goroutine remains owned by the closed window.|

T04. The shared lifecycle tests MUST use a backend executable distinct from the GUI executable.
They MUST cover missing binaries, replaced binaries, build mismatch, peer rejection, startup timeout, and a daemon that exits during initialization.
Restart MUST require the declared user decision when it can interrupt another client's work.
Two simultaneous launches of one endpoint MUST converge on one valid daemon through the existing startup rules.
Simultaneous launches of different endpoints MUST create independent sessions without sharing connections or credentials.
Daemon remote fixtures MUST accept equivalent `https` and `wss` base URLs and join `/rpc` correctly.
They MUST reject insecure schemes, malformed URLs, unsafe token files, and exposed tokens.
Invalid credentials or TLS configuration MUST enter Failed without an automatic retry.
An `Authorization` header MUST take precedence over the query token, including when the query token differs.

T05. All relevant existing clients MUST pass after lifecycle and RPC package extraction.
The TUI, web relay, app-server, and command entry points MUST use the selected shared policy.
Tests MUST prove the absence of divergent old connection paths through caller behavior and dependency checks.
No test may weaken the journal or recovery contract to accommodate the GUI.

## Requests and authentication

T06. Approval and question tests MUST use two connected clients.

|Case|Required observation|
|---|---|
|Both clients answer one request|Exactly one valid answer wins. Both clients close the same request identity.|
|One client sends invalid answer text, then corrects it|Its card and sender survive. The corrected response uses the same request ID and original deadline.|
|Correction races another valid answer or timeout|One outcome wins. A delayed warning cannot reopen the card.|
|A request belongs to a child thread|The card identifies its owner, and the response reaches that owner.|
|A request belongs to another root thread|The UI offers an explicit navigation action without changing the active thread automatically.|
|The last subscriber disconnects|Approval and question results follow the existing no-client rules.|
|A resolved request appears in an old snapshot|The client does not resurrect an actionable card.|
|Load approval details larger than the mailbox|Bounded detail reads preserve the full decision context without reconnect loops or premature approval.|
|An IME consumes Enter or Escape|The request and composer receive no unintended submit or cancel action.|
|A question has options and free text|The response preserves declared option order and the bounded free-text value.|

T07. Each supported provider authentication flow MUST have a scripted success, refusal, expiry, cancellation, and transport-failure case.
The native UI MUST complete every flow that the application claims to support.
MCP authentication MUST have the same coverage for its supported flows.
Tests MUST cover incorrect state, an unexpected callback origin, and a reused authentication operation.
No test artifact, diagnostic, journal record, or exported report may contain a raw credential.

Direct API-key login MUST omit `callbackMode` and create no browser, listener, or OAuth poller.
It MUST remain available on a remote endpoint without selecting a device or manual OAuth flow.
Its exact accepted key MUST enter only the returned transaction's key submission path.
Tests MUST reject missing or mismatched callback modes before any authentication effect.
MCP tests MUST distinguish same-name user, project, and discovered entries by their effective source and inventory version.
A configuration change between status pages MUST return `stale_revision` without attaching old runtime state to a replacement row.
A capable daemon omitting runtime provenance MUST be rejected as incompatible.

At least one supported browser flow and one supported device flow MUST also pass against their real service before release.
A provider's unavailable test account MUST be recorded as missing evidence, not a successful simulated login.

## Component and keyboard fixtures

T08. The test inventory MUST enumerate the 43 source components from the [component mapping](native-components.md).
Each mapping MUST name a native behavior test or a documented native manual test.
Grouping several components in one test is valid only when each behavior has an independent assertion.

Required component scenarios include the following cases.

- Open each panel from its documented command and keyboard route.
- Preserve the opener's focus after save, cancel, and failure.
- Keep request actions reachable while their details exceed the window height.
- Show complete model, effort, approval, and sandbox state in the declared status details.
- Navigate a tree containing a failed child, a running child, and an archived branch.
- Reject rewind during a running turn and explain that a completed rewind does not undo files.
- Keep a queued follow-up's text and attachments ordered during edits and removals.
- Preserve a draft when a send is rejected for size or unsupported image input.
- Default Run on to the displayed thread's endpoint and create through an explicitly chosen different endpoint.
- Recall sent text without losing the saved draft.
- Keep a stale file-completion result from accepting or sending a different token.
- Report unknown commands locally while preserving literal paths and escaped slash input.
- Keep each operation's endpoint and workspace visible during remote operations.
- Require explicit consent before sending the previewed failure report.

T09. Keyboard tests MUST cover each shortcut at the correct focus owner.
Global shortcuts MUST NOT consume text editing, terminal input, or active IME composition.
The double-Escape interrupt window MUST use a controllable clock.
The implementation MUST test focus restoration after the opener is removed from the model.

## Text, figures, and images

T10. Document fixtures MUST combine English, CJK, combining marks, emoji sequences, bidirectional text, long tokens, and mixed font fallback.
The expected copy result MUST come from the document's source text and declared copy policy.
It MUST NOT come from the rendered glyph buffer.

|Case|Required observation|
|---|---|
|Append text while a range is selected|The logical range remains stable and copies the expected text.|
|Select across a code block and prose|Style boundaries do not split or reorder the copied range.|
|Copy CRLF source on each OS|Source actions preserve original line endings. Generated separators and clipboard conversion follow the explicit copy contract.|
|Select across an unloaded page|The operation fetches bounded pages and preserves the full logical selection.|
|Insert older history above the viewport|The anchored text remains at its prior viewport offset.|
|Change font scale or monitor DPI|The document reflows without changing the logical caret or selection.|
|Stream an unfinished fence or table|The live block remains readable and converges to the final parse.|
|Inspect a large spilled output|Only requested pages become resident. Export includes all pages.|
|Reuse an item ID for two spilled outputs|Each turn reads and exports its own complete masked output through its snapshot reference.|
|Change branches during output export|The export retains its frozen path and content digest, or fails explicitly without publishing mixed output.|
|Cancel a full export|The operation stops, closes files, and removes its incomplete temporary output.|

T11. Every supported Mermaid, D2, nomnoml, SVG, and math construct MUST have a valid rendering fixture.
The fixture corpus MUST include nested groups, multilingual labels, edges, transforms, and the declared math environments.
It MUST include a valid figure whose semantic labels exceed the worker envelope limit but fit the declared result budget.
Malformed and adversarial cases MUST cover deep nesting, cyclic references, oversized dimensions, excessive work, and forbidden external resources.
The worker MUST stop at its hard deadline and release its process resources.
One failed figure MUST NOT block text selection, message input, or another figure's result.

Pixel comparison MAY detect rendering drift.
It MUST accompany structural assertions for visible labels, bounds, links, copy source, and accessibility text.
A source-code block MUST NOT satisfy a valid diagram's rendering assertion.

T12. Image tests MUST cover restart, remote threads, cached bytes, removed source paths, MIME mismatch, and decompression bombs.
The daemon MUST reject an image reference that does not belong to the requested item.
The decoder MUST enforce decoded dimensions and memory limits before allocating the complete image.
Copy, save, zoom, and accessible descriptions MUST work for each supported image format.

## Workspace and terminal authority

T13. File operations MUST run against local, thread-box, and daemon-in-box fixtures.
The same textual path on two endpoints MUST never identify the same resource implicitly.
An offline remote fixture MUST refuse local-filesystem fallback while another endpoint remains operational.
Tests MUST cover symlink escape, traversal, stale revisions, concurrent writes, removed roots, cancellation, and disconnected runners.
A refused or conflicted write MUST leave the previous file intact.
The UI MUST retain unsaved text and show an explicit recovery action.
Repeatedly opening and closing more than eight idle workspaces MUST reuse the released slots.
Closing one of two views sharing a handle MUST preserve the other view's grant.
Close with a terminal attachment or dispatched mutation MUST return `busy` without cleanup side effects.
After detach and operation reconciliation, close MUST remove watches, cursors, and uncommitted uploads without stopping other terminals or deleting files.
Reads racing successful close MUST cancel or fail without publishing stale results.
One-shot shell fixtures MUST prove that the same command runs on the selected thread runner with the granted execution right.
They MUST reject daemon-host substitution and preserve unknown outcomes without automatic repetition.

T14. Terminal tests MUST use an actual PTY and the production terminal parser.
They MUST cover shell input, interactive applications, resize, Unicode, alternate screen, cursor movement, colors, and bracketed paste.
They MUST cover output wrap, missing sequence detection, reconnect policy, and closed-terminal input rejection.
One terminal query with multiple readers and a reconnect MUST produce exactly one automatic reply from the daemon.
Forced short writes and lost runner acknowledgments MUST preserve each consumed input prefix exactly once.
They MUST expose uncertain input and reject further writes across reconnect and writer replacement.
Terminal output MUST NOT execute application commands or silently write the system clipboard.
The client MUST distinguish a live terminal from the existing one-shot shell-output component.
Equal terminal or thread identifiers on two endpoints MUST NOT share attachments, cursors, output, or input.

|Case|Required observation|
|---|---|
|Resize with unread output on writer and read-only attachments|Both replicas consume the same ordered byte and resize history and match the authoritative VT state.|
|Commit several resizes at one byte offset|Every record applies in ordinal order without final-size-only substitution.|
|Queue more than 64 resizes at one offset|Bounded reads advance the resize cursor even with zero output bytes, then resume output without omissions.|
|Evict a required resize record while nearby bytes remain|The reader receives `replay_gap` and restores both cursors through a checkpoint.|
|Refresh while all four attachment slots are occupied|`checkpoint/open` replaces the requesting attachment's checkpoint without consuming a slot or changing ownership.|
|Refresh the only reader of a finalized terminal|Recovery preserves retained output and does not detach the last reader or extend its fixed expiry.|
|Refuse refresh during a checkpoint read or at the checkpoint budget|The old checkpoint and attachment remain usable. No partially published replacement appears.|
|Lose or reorder resize acknowledgments|An exact retry appends no extra record. A stale response cannot rewind replica dimensions.|
|Resize during an incomplete UTF-8 or terminal escape sequence|Checkpoint and replay retain equivalent parser continuation and geometry.|
|Stop while shutdown emits a final output tail|Existing readers collect through the final byte and resize cursors before declaring completion.|
|Reach the drain deadline before EOF|The UI reports `outputTruncated:true` and never claims complete output.|
|Poll, attach, or close again after finalization|The fixed output-retention deadline does not move.|
|Expire retention before a reader reaches both final cursors|The view keeps collected output, marks the missing tail, and releases local attachment and workspace interests.|
|Detach the last finalized attachment|Retained state is released and cannot be reached through a reused terminal identity.|
|Receive an uncertain remote resize outcome|Further input is refused while output and explicit Stop remain available.|

A Windows ConPTY test MUST exercise the same input and resize behavior as the Unix PTY test.
Remote tests MUST show that the shell runs in the selected thread's actual execution environment.

## Native platform evidence

T15. Each supported OS and architecture MUST have build and package validation.
Native interaction checks MUST run on the platform they claim to cover.
Linux evidence MUST distinguish Wayland and X11.
An emulated or virtual display result MUST identify that environment.

|Platform group|Required manual evidence|
|---|---|
|Windows|Korean, Japanese, and Chinese IMEs under P05, NVDA, Narrator, UI Automation inspection, ConPTY, menus, dialogs, image clipboard, and mixed DPI.|
|macOS|Korean, Japanese, and Chinese input under P05, VoiceOver, accessibility inspection, native menus, dialogs, clipboard, and display scaling.|
|Linux Wayland|IBus and Fcitx5 language pairs under P05, compositor protocol identity, Orca, portals, clipboard, and scaling.|
|Linux X11|IBus and Fcitx5 language pairs under P05, Orca, native dialogs, clipboard, and scaling.|

T16. IME tests MUST cover composition, candidate placement, replacement, deletion, commit, cancellation, and a focus change during composition.
The test MUST prove that composition Enter does not also send a message.
It MUST prove that composition Escape does not interrupt a turn or close a dialog.
Rendering precomposed CJK text alone is not an IME test.
Tests MUST also cover selection replacement, supplementary characters, emoji insertion, rejected commits, and field changes during composition.
The actual input-method and browser or terminal matrix comes from [Input text](../../dal/spec/input-text.md#i12-prove-cross-client-behavior).

T17. Screen-reader tests MUST read the shell, endpoint lane labels, phase words, thread list, transcript, editor, requests, dialogs, and terminal status.
They MUST distinguish equal thread titles and identifiers by their endpoint labels.
They MUST invoke actions and edit text through the OS accessibility API.
Text-range tests MUST cover selection and virtualized document navigation.
The live status channel MUST announce meaningful state changes without reading every streamed token.

T18. Native-handle tests MUST invalidate and recreate the platform view.
Old callbacks MUST not access a destroyed window or a newer handle generation.
OS callbacks MUST not mutate Gio widgets directly or block on network work.
Native dialogs MUST preserve cancellation and return focus to a valid owner.

## Visual and motion evidence

T19. The visual fixture matrix MUST cover every theme, the narrow layout, the wide layout, high text scale, and high contrast.
It MUST cover 100%, 125%, 150%, and 200% display scaling where the OS exposes those values.
Reflow MUST remain usable at 320 logical pixels without whole-window horizontal scrolling.
Code, diagrams, diffs, and terminal inspection follow their declared local overflow rules.

The matrix MUST verify the single-point priority, state words, focus ring, minimum targets, title truncation, and stable duration width.
It MUST show merged endpoint lanes, all seven phase words, readable Idle, and no endpoint accent fills.
The renderer MUST use the declared font fallback and registered icon glyphs.
The application MUST NOT fetch a font when it first displays Hangul.

T20. Reduced-motion tests MUST stop the breathing dot and elapsed-time tick as the native design requires.
An unchanged inactive window MUST not schedule a permanent redraw loop.
A caret, active interaction, or declared animation MAY request frames while it is active.
Repeated invalidation requests MUST not create an unbounded work queue.

## Performance targets and measurements

The following numbers are acceptance targets, not measured results.
They constrain the implementation and do not describe the current repository.

T21. The benchmark record MUST name one reference machine and preserve that machine's configuration for comparisons.
It MUST record CPU model, memory, GPU, driver, OS, display backend, resolution, and scaling.
Results from different machines MUST not be combined into one percentile.
Each result MUST include cold and warm runs where caching changes the operation.

|Fixture|Required workload|Target|
|---|---|---|
|Idle window|No active turn, caret, animation, or pending work for 60 seconds.|No application-scheduled frame loop. Average process CPU below 1% of one logical core.|
|Scroll transcript|100,000 blocks with mixed prose, code, and collapsed tools.|Frame CPU time p95 at most 16.7 ms and p99 at most 33.4 ms.|
|Type while streaming|A 256 KiB per second text stream for 60 seconds.|Input-to-frame latency p95 at most 50 ms. No missed or duplicated input.|
|Open history|20,000 turns with paged item bodies.|First usable page within 1 second on the local reference fixture. Work is bounded by requested pages.|
|Read output|A 1 GiB spilled command output.|Resident output storage stays within the document cache bound. Export returns the complete content.|
|Inspect diff|100,000 changed lines.|Visible-row work and selection remain bounded by the viewport and requested pages.|
|Cancel a figure|A renderer input that exceeds its declared deadline.|The process exits within the worker termination grace period. The editor remains usable.|
|Repeat navigation|Open and close 1,000 distinct thread views.|Subscriptions, goroutines, native objects, and caches return to their declared bounds.|

T22. CPU time, wall time, Go heap, process memory, worker memory, and GPU memory MUST be reported separately.
The measurement MUST include the renderer workers when reporting total application memory.
No fixed memory figure may exclude growing caches or retained transcript text without naming that exclusion.
The cache and queue bounds are defined by [architecture](native-architecture.md), [protocol](native-protocol.md), and [documents](native-documents.md).

An allocation claim MUST identify the measured operation and steady-state conditions.
The implementation MUST NOT describe the entire GUI as allocation-free.
Performance improvements MUST preserve the correctness fixtures above.

## Build and delivery gates

T23. Every implementation commit MUST run the applicable repository checks and the required `just verify` command.
Desktop implementation changes MUST also run the dedicated desktop verification target from [platforms](native-platform.md).
The target MUST include tagged code in formatting, vetting, linting, race tests, and builds.
Pure state, protocol, and document tests MUST run without a display server.
Native window tests MUST run separately with an actual supported display backend.

T24. Package tests MUST cover a fresh install, launch without development tools, upgrade, rollback, and uninstall.
They MUST verify the bundled backend's identity and the absence of web-engine runtime dependencies.
They MUST verify asset and license inclusion, package signatures, and the declared user-data retention behavior.
The headless release and remote box artifacts MUST pass their existing build checks.

`just desktop-package` assembles the `.deb` on Linux, the universal `.dmg` on macOS, and the x64 or Arm64 `.msi` on matching Windows hardware.
The macOS recipe requires Xcode, `DALGONA_MACOS_SIGN_IDENTITY` for a Developer ID Application identity, and `DALGONA_MACOS_NOTARY_PROFILE` for an existing `notarytool` keychain profile.
It checks both Mach-O slices, app and image signatures and notarization tickets, and resources in the mounted image.
The Windows recipe requires WiX 5 or later and Windows SDK SignTool.
`DALGONA_WINDOWS_SIGN_SHA1` selects a signing certificate in the current user's `My` store.
`DALGONA_WINDOWS_TIMESTAMP_URL` supplies an HTTPS RFC 3161 endpoint.
It checks the executable architectures, MSI platform and schema, extracted payload bytes, and timestamped Authenticode signatures.
Before signing the MSI, it also checks the built package's Start menu shortcut, toast properties, and registry registration and cleanup.
Both recipes take `DALGONA_PACKAGE_VERSION` as `major.minor.patch` or derive it from an exact numeric `v` tag.
Both refuse missing signing inputs. CI checks their sources without signing or publishing.

Assembly checks do not count as installation evidence.
Use clean systems from the platform matrix for offline launch from system menus, upgrade, rollback, uninstall, and user-data retention.
On Windows, verify the Start menu shortcut and COM registration on install.
Verify toast activation before and after GUI exit.
Verify removal of the shortcut and registration on uninstall.
The assembly recipes do not run these system tests.

The implementation MUST update existing specifications when it changes their runtime behavior.
It MUST update the dependency decision and required notices when it introduces a dependency.
It MUST preserve the existing documentation link and style checks.

## Implementation order

T25. The implementation MUST follow dependency order while keeping each change independently reviewable.
The following work packages are a completion sequence, not separate reduced-scope releases.

|Order|Work package|Exit evidence|
|---|---|---|
|1|Extract shared lifecycle and separate RPC server implementation.|Existing clients pass. Package boundaries and backend identity are verified.|
|2|Implement capabilities, bounded history, and subscription lifecycle.|Snapshot and live-event fixtures pass under reconnect and backpressure.|
|3|Build the Gio shell, endpoint set, design tokens, state owners, and native input.|Two endpoint sessions drive independent threads and composers on each OS.|
|4|Implement every mapped web component and panel.|All C01-C43 behaviors have evidence. No action is inert.|
|5|Complete documents, figures, math, diffs, and recorded images.|Valid content renders. Selection, export, and hostile-input fixtures pass.|
|6|Complete authentication, remote management, files, and real terminals.|All C44-C51 workflows complete against their declared owner.|
|7|Complete OS integrations and accessibility bridges.|Platform, CJK IME, and screen-reader checks pass.|
|8|Package the application and run the complete acceptance matrix.|The complete release has reproducible evidence and no deferred required path.|

The native release MUST NOT be marked complete until every work package passes.

## Cross-client Unicode inputs

T26. GUI, TUI, web, and daemon implementations MUST share the [input corpus](../../dal/spec/input-text-cases.json) and [input contract](../../dal/spec/input-text.md).
The corpus is a specification artifact. Its structural validation does not prove runtime conformance.
Each client MUST test production input adapters, exact round trips, transactional edits, recovery, and the platform behavior it claims.
The daemon MUST test strict decoding, whole-request refusal before effects, answer correction, and unchanged journal bytes.

Tests MUST distinguish byte counts, scalar quotas, UTF-16 offsets, grapheme boundaries, shaped geometry, and terminal cells.
They MUST cover the shared adversarial cases and the pinned upstream segmentation corpus.
Question tests MUST accept 2000 supplementary scalars despite their 4000 UTF-16 units, and reject 2001 scalars without truncation.
Transport tests MUST split UTF-8 sequences across reads and grapheme clusters across valid deltas, then verify exact reconstruction.
Client tests MUST reject stale completions and preserve private-use characters beside attachment chips.
Search tests MUST keep `ab ` and `abc de` local while sending eligible ` abc ` unchanged.
Required model question fields MUST reject blank or formatting-only content without trimming any accepted value.
These eligibility checks MUST NOT redefine whitespace-only free-form answers as empty.
Every declared platform MUST supply actual CJK composition evidence in addition to headless test results.
