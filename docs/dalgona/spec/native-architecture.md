# Native client architecture

This document specifies the Gio desktop client. It does not certify an implemented client or completed acceptance checks.
The source baseline is `2c036c60b379e7718255a23ebd2c5079d76ec50c`.

The native client uses Go and Gio. The daemon remains a separate process and owns the agent, journal, tools, providers, and runner bindings.
The desktop application uses the existing JSON-RPC transport through a shared endpoint lifecycle package.

Read [the protocol](native-protocol.md), [components](native-components.md), [design](native-design.md), [documents](native-documents.md), and [platform contract](native-platform.md) together.

## A01. Existing contracts and required changes

The following table distinguishes existing behavior from required implementation work.

| Area | Existing behavior | Required native work |
| --- | --- | --- |
| Client wire | `rpc.Dial` returns `*rpc.Conn`. | Reuse this transport and its request cancellation. |
| RPC dependencies | `internal/dal/rpc` holds transport, codecs, and wire types. Host-dependent handlers are in `internal/dal/rpcserver`. | Keep host-dependent handlers out of the wire package. |
| Startup | Every local client opens a `client.Session` with a `client.Backend` and calls `Connect`. | Pass the bundled or selected backend instead of the GUI executable. |
| Build identity | `client.ResolveBackend` hashes the selected backend file, and `Connect` compares the daemon against it. | Keep the GUI executable out of the comparison. |
| Remote transport | `client.Remotes` owns the remote table and the bridge. `remote.Binder` keeps thread runner binding. | Add endpoint management and cloud operations to the lifecycle owner. |
| Desktop | No Gio executable exists. | Add the executable and complete every required component. |
| History | Pages can require a complete journal read. | Add bounded indexed snapshots and an ordered event stream. |
| Files | Existing `fs/*` methods access the daemon host. | Add thread runner routing and explicit workspace authority. |
| Terminal | `command/exec` rejects TTY, stdin streaming, and size. | Add real PTY sessions through the bound runner. |
| Images | Transcript items expose `dalgona-blob:` references. | Add authorized image retrieval. |
| Authentication | Several provider and MCP flows require terminal orchestration. | Add structured native transactions for every supported flow. |

Current evidence comes from [the lifecycle session](../../../internal/dalgona/client/session.go), [client transport](../../../internal/dal/rpc/conn.go), and [server registration](../../../internal/dal/rpcserver/server.go).
See [remote endpoints](../../../internal/dalgona/client/remotes.go), [remote ownership](../../../internal/dalgona/remote/remote.go), and [product extensions](../../../cmd/dalgona/remote.go).

The desktop MUST NOT call agent, journal, provider, tool, or runner implementations directly.
Shared language does not change process ownership or authorization.

## A02. Whole architecture comparison

Candidate A launches the bundled backend as `dalgona app-server --listen stdio://`.
The application reuses `rpc.Dial` over the child process pipes.
Remote invocation places the selector before the command: `dalgona --remote NAME app-server --listen stdio://`.

Candidate A preserves existing startup behavior with little extraction.
Its helper process adds another failure boundary and exposes startup diagnostics through stderr.
Its complete design still needs history, authentication, filesystem, and PTY extensions.

Candidate B shares endpoint lifecycle code and connects directly to trusted local IPC or the remote bridge.
It preserves a separate daemon while removing the proxy process from desktop operation.
Its complete design requires deliberate package separation and migration of existing entry points.

The [native client decision](../decisions/native-client.md) records the qualitative comparison and selection.

Candidate B is selected. Typed lifecycle outcomes and one shared startup policy justify its extraction cost.
Candidate A is rejected as the desktop architecture because its extra process retains a separate startup failure boundary.
The existing app-server remains supported for external clients.

## A03. Types and ownership

These are proposed Go responsibilities, not claims about existing exported types.
Concrete types MUST precede lifecycle functions in their implementation files.

| Type | Owner | Invariant |
| --- | --- | --- |
| `Endpoint` | `client` | One local home or one configured remote, with an immutable identity revision. |
| `BackendArtifact` | `client` | Absolute executable path, verified build identifier, and selected product. |
| `EndpointSession` | `client` | One endpoint, its first-connection policy, and its current connection generation. |
| `DaemonIdentity` | `rpc` | Observed daemon instance, backend build, protocol revision, and platform. |
| `BuildMismatchError` | `client` | Running identity and selected backend identity remain available as typed fields. |
| `UnknownOutcomeError` | `rpc` | A mutation was dispatched without a conclusive response. |
| `SessionState` | `gui` | Endpoint-scoped thread projections, pending decisions, and connection state. |
| `ThreadProjection` | `gui` | One stream identity, applied ordinal, active branch revision, and authoritative items. |
| `DraftState` | `gui` | One window, endpoint, and draft identity, with an optional target thread. |
| `DocumentState` | `gui/document` | Document version, selection, folds, and source ranges. |
| `TerminalView` | `gui/terminal` | One terminal identity, screen checkpoint, output and resize cursors, attachment, and writer lease. |
| `WindowState` | `gui` | Gio widget state, focus, layout, and accessibility targets for one window. |

Endpoint identity MUST include the product home for local connections.
Remote identity MUST include the service, configured remote, box identity, and configuration revision.
Daemon instance identity MUST change after every daemon restart.
Display names MUST NOT serve as cache keys or authority tokens.

Drafts and preferences belong to the GUI. Credentials, provider sessions, and workspace authority belong to their backend owners.
The GUI MUST NOT persist bearer credentials, OAuth codes, terminal input, or approval answers in its restore state.
Windows do not share a mutable composer draft merely because they display the same thread.
Each send intent carries an immutable draft revision from its owning window.

## A04. Package boundaries

The implementation MUST retain one Go module and the shared core defined by [two products](../../dal/decisions/two-products.md).
The executable belongs at `cmd/dalgona-gui`.
Application code belongs under `internal/dalgona/gui`.
Its `document`, `figure`, `terminal`, and `platform` children own their named rendering or operating system decisions.

| Package | Required responsibility | Forbidden dependency |
| --- | --- | --- |
| `internal/dal/rpc` | Framing, wire types, codecs, connection, and client methods. | Agent, journal, auth, provider, tool, or runner implementation. |
| `internal/dal/rpcserver` | Host-dependent method handlers and server projection. | Gio or frontend state. |
| `internal/dalgona/client` | Trusted endpoint discovery, launch, restart, bridge, and client-owned cloud operations. | Agent, journal, tool, or thread runner binding. |
| `internal/dalgona/daemon` | Daemon runtime, host assembly, recovery, and server lifetime. | GUI implementation. |
| `internal/dalgona/remote` | Thread binding, box ledger, archive policy, pull, and runner garbage collection. | GUI implementation. |
| `internal/dalgona/sandboxsvc` | Service construction and service-level transport capabilities. | Journal or thread projections. |
| `internal/dalgona/gui` | Native application state and user interaction. | Backend implementation packages. |

`client` is justified by one invariant: connection trust and backend identity remain consistent across startup, reconnect, restart, and endpoint changes.
It MUST NOT wrap every RPC method or become a second client SDK.
The GUI MUST call `rpc.Conn` for protocol operations.

Move service construction and caching from `remote.Binder` into the existing sandbox service boundary.
Both lifecycle transport and thread binding MUST consume that owner.
There MUST NOT be separate service registries with divergent credentials or connection rules.

Move the trusted local dial, socket verification, and process-launch code into `client`.
Daemon listener setup MUST reuse the same ownership checks where their semantics match.
Keep server runtime and recovery inside `daemon`.
Moving only `cmd/dalgona` functions is insufficient because their imported packages retain server dependencies.

Move core-dependent RPC handlers and extension registration into `rpcserver` without copying wire types.
The implementation MUST inspect package dependencies after migration.
It MUST NOT claim a lean executable from an unmeasured linker assumption.
The server package registers its method map during initialization.

The boundary decision MUST update `.golangci.yml` and the decision index in the same change.
Existing TUI imports remain limited to wire and explicitly approved shared state contracts.

## A05. Lifecycle API and migration

The concrete `EndpointSession` exposes `Connect`, `Reconnect`, `Restart`, and `Close`.
`Connect` may launch a local daemon before its first successful connection.
`Reconnect` only dials the known endpoint.
`Restart` requires an explicit user action and the identity being replaced.
`Close` releases this client's transport and never retires the shared daemon.

Endpoint management exposes `Endpoints`, `SaveRemote`, `CloudUp`, `CloudPull`, and `CloudDown` through the same lifecycle owner.
Those operations act on the selected local configuration and remote service.
They MUST NOT execute against whichever daemon currently occupies a conversation tab.
Their typed requests carry operation identifiers and explicit target identities.

`CloudDown` requires a fresh confirmation naming the box and destructive effect.
`CloudUp` copies credentials only through an explicit separate choice.
Cloud operations retain the ownership rules in [runners.md](runners.md).

Every extraction MUST migrate all affected callers in the same change.
This includes TUI launch, app-server, web launch and reconnect, bridge, diagnostics, and relevant CLI commands.
Old implementations and compatibility forwarding packages MUST be removed.
Tests MUST exercise the extracted owner rather than preserve duplicate startup paths.

Migration order is:

1. Separate transport and server dependencies with unchanged wire behavior.
2. Extract trusted lifecycle and service transport ownership.
3. Migrate every existing lifecycle caller and remove old entry points.
4. Add negotiated protocol extensions and independent conformance fixtures.
5. Add the Gio executable and complete its release gates.

Implementation commits MAY divide this sequence. Each landed commit MUST preserve existing products and their package boundaries.
The documentation change authorizes no claim that these migrations already exist.

## A06. Startup, identity, and restart

The GUI MUST resolve its bundled or explicitly selected backend before attempting startup.
It MUST hash that backend artifact, never the GUI executable.
`client.Build()` hashes the running process image, which is right only for a daemon or a CLI that is its own backend.
Passing the GUI executable as the backend would also try launching that executable with the `daemon` command.

Startup MUST preserve home permissions, socket ownership, peer credentials, and platform ACL checks from [daemon.md](daemon.md).
An untrusted endpoint MUST fail before initialization or credential transmission.
An unsupported peer verification mechanism MUST fail closed.
GUI process sandboxing MUST NOT bypass the existing peer sandbox checks.

Initial local connection states are `resolving`, `checking`, `starting`, `initializing`, `reconciling`, and `ready`.
Failure states are `unavailable`, `untrusted`, `incompatible`, and `offline`.
Only successful initialization and required capability validation permit reconciliation.

A build mismatch MUST show the running and selected backend builds.
The GUI MUST offer an explicit restart action with the affected endpoint and active-work consequences.
It MUST NOT retire a daemon during automatic reconnect or because a window opened.
Remote restart requires a supported remote lifecycle action and its own confirmation.

Missing native capabilities produce `incompatible` with a backend update action.
The application MUST NOT display apparently complete controls backed by unavailable methods.
Restart MUST recheck identity immediately before retirement to avoid retiring a replacement daemon.

## A07. Connection ownership and backpressure

One application process owns one live connection per selected endpoint.
Windows share endpoint state through the application owner.
A window MUST NOT open an additional daemon connection merely to display the same thread.
The application counts interested views by endpoint and thread identity.
One shared projection and history handle serve every interested view of that thread.
Each view retains its own scroll position, selection, and pagination cursor.

The connection reader validates framing and transfers owned immutable messages to the session worker.
The session worker alone mutates protocol state.
The Gio event loop alone mutates widgets, focus, layout operations, and image resources.
Workers publish immutable view snapshots and call window invalidation.
They MUST NOT call widget layout or retain a frame's Gio operations.

| Queue or cache | Required bound | Full behavior |
| --- | --- | --- |
| RPC dispatch | Existing 256 messages plus a 16 MiB aggregate budget. | Close the connection and reconcile. |
| Native session mailbox | 256 events or 8 MiB, whichever arrives first. | Close the affected endpoint and preserve pending mutation outcomes. |
| Pending mutations | 64 per endpoint. | Reject additional actions with visible busy state. |
| Active request workers | 16 per endpoint. | Queue within the pending bound or return busy. |
| Published view snapshots | One latest snapshot per window. | Replace the superseded snapshot. |
| Live thread subscriptions | 8 per connection. | Release an unreferenced thread or visibly refuse the new subscription. |
| Shared CPU pixel cache | 64 MiB per application. | Evict unreferenced image, figure, glyph, or thumbnail tiles. |

Windows share one CPU pixel store through references.
They MUST NOT duplicate decoded pixels in separate per-window caches.
The document contract separately bounds worker decode memory and GPU textures.

RPC queue byte accounting is new work. The existing message count alone does not bound memory sufficiently.
Stream text MAY coalesce only when identity, stream, field, and adjacent ordinal ranges match.
Coalescing MUST preserve the snapshot barriers in [R04 and R05](native-protocol.md#r04-one-ordered-delivery-stream).
The session MUST retain received envelope boundaries while a snapshot cut is pending.
Coalescing MUST preserve exact text order and the last ordinal.
Approvals, questions, completions, failures, and mutation responses MUST never be discarded or replaced.
If every subscription has an interested view, opening another MUST report capacity without evicting another window's thread.

Queue overflow MUST terminate the affected stream with a visible reason.
The client then obtains an authoritative snapshot.
It MUST NOT render a partial event history as complete.

## A08. Reconciliation and cancellation

Native thread views MUST use the negotiated ordered stream in [native-protocol.md](native-protocol.md).
Existing `thread/status/changed.seq` values order status messages only.
They MUST NOT become replay cursors, journal offsets, or snapshot watermarks.

Each connection generation owns its calls, subscriptions, pending decisions, and caches.
Responses from a previous generation MUST be ignored after their outcome is recorded.
Changing endpoint identity clears credentials in transit, request state, workspace handles, and terminal leases.
Drafts remain partitioned by their original endpoint.

On disconnect, the GUI freezes rendered authoritative content and marks active work as unverified.
It cancels pending request contexts and retains typed unknown outcomes for dispatched mutations.
It retries connection after one, two, and four seconds, with a 30-second deadline per attempt.
After three failures, it requires an explicit reconnect action.
It never restarts a daemon as a retry mechanism.

Reconciliation initializes the new connection, validates identity, opens fresh snapshots, and replaces each thread projection atomically.
Unsent drafts remain editable.
Dispatched prompts MUST NOT be sent again automatically.
The GUI resolves uncertain operations through recorded operation identifiers where supported.
Legacy mutation uncertainty requires inspection and explicit user action.

Cancelling a client context cancels waiting, not an acknowledged side effect.
Turn interruption MUST use `turn/interrupt`.
Terminal termination MUST use `dalgona/terminal/close`.
Terminal attachment release MUST use `dalgona/terminal/detach` without implying process termination.
Authentication cancellation MUST use its transaction cancellation method.

## A09. Authorization and path domains

Every displayed path MUST carry its domain: GUI local, daemon host, or thread runner.
The GUI MUST never translate a remote path through local filesystem operations.
Remote `/workspace` paths remain POSIX paths even on a Windows GUI host.
File selection sends bytes when the backend cannot access the selected local path.

Workspace handles bind the endpoint, thread, runner identity, root, rights, and binding revision.
The daemon validates these fields before every operation.
Runner rebinding invalidates old handles before any filesystem mutation.
User filesystem actions never inherit an agent approval silently.

The shared client owner MUST count workspace interests across windows, file views, completion, commands, and terminal attachments.
Closing one view releases only its interest. A terminal retains its workspace reference until its attachment is detached.
The final interest MUST close the server handle through `dalgona/workspace/close` after dependent operations finish or reconcile.
A `busy` close MUST preserve the handle and its dependent work. It MUST NOT trigger forced terminal or operation cancellation.
Thread unsubscribe MUST NOT revoke a workspace still owned by another view or terminal.

Authentication transactions keep PKCE verifiers, refresh tokens, and API keys within the credential owner.
Native browser opening uses the operating system browser.
Remote callback limitations MUST be represented explicitly by the selected flow.
No embedded web view or secret transfer file is required.

The GUI exposes masked failure records through the protocol.
It MUST NOT read daemon journals or credential files for convenience.
Diagnostics retain their read-only and no-daemon-start contract.
Failure reporting retains explicit consent and the restrictions in [report.md](report.md).

## A10. Restore, close, and shared state

Restore data contains endpoint references, open thread identifiers, drafts, panel geometry, and local appearance preferences.
Persist it atomically within the GUI state directory with private permissions.
Restore data MUST carry a schema version and bounded collection sizes.
The manifest is limited to 1 MiB, 32 endpoint references, 32 thread references, and 64 draft references.
Draft bodies live in separate private files, with 1 MiB per draft and 16 MiB combined.
Draft persistence keys include the stable window identifier, endpoint identity, and draft identifier.
Restoring two windows cannot overwrite either window's draft for the same thread.
Draft attachment blobs use a separate 64 MiB combined budget and content digests.
A source file can remain referenced only while its identity and digest still match.

At a persistence limit, editing continues in memory with an explicit unsaved indicator.
The GUI offers export, removal of another saved draft, or explicit discard.
Window closure requires a decision before losing unsaved content.
It MUST NOT truncate a prompt, discard attachments, or evict a draft silently.
Large pasted attachments retain their normal send limits and receive a visible storage error if persistence lacks capacity.

Restoring a window never submits a prompt, repeats a mutation, grants permission, or launches a terminal.
Stale threads remain visible as unavailable references until the user removes them.
Restored workspace views require fresh authority handles.
Terminal identifiers from another daemon instance are expired.

Closing a thread view releases that view's interest and its private pagination work.
Only the final interested view releases the shared history handle and calls upstream `thread/unsubscribe`.
Another window's active view therefore retains its subscription and decision responders.
These releases do not archive the thread or interrupt its turn.
Closing the last client may decline pending approvals or finish questions with no client.
That current backend behavior MUST remain visible when closing affected work.

Application exit cancels owned authentication flows and closes explicitly owned terminals after the required user decision.
Stop retains its reader until final output is collected, the user releases the view, or output retention expires.
Expiry releases local attachment and workspace interests while keeping collected output and the visible loss boundary.
The terminal owner detaches before releasing its final workspace interest.
Transient disconnection uses terminal lease recovery instead of immediate process termination.
The shared daemon remains available to TUI, web, app-server, and other clients.

Only proven state invariants MAY move into shared Go code.
Examples include ordered projection validation and pending-request resolution.
Such extraction MUST include real TUI and native callers in the same change.
Bubble Tea models, Gio widgets, browser stores, and presentation reducers remain frontend-specific.
The web client shares protocol fixtures and behavioral requirements, not Gio state.

## A11. Acceptance requirements

All sections above are release gates. Acceptance MUST additionally prove the following outcomes.

| Requirement | Required evidence |
| --- | --- |
| A01 | Native dependency inspection contains no GUI imports of agent, journal, provider, tool, or runner implementations. |
| A02 | The shipped GUI uses the selected direct lifecycle architecture. |
| A03 | Endpoint changes cannot deliver stale responses, secrets, or filesystem actions into another endpoint. |
| A04 | Headless products build without Gio or desktop native libraries. |
| A05 | TUI, app-server, web, bridge, and GUI exercise one lifecycle implementation. |
| A06 | Backend hash selection, mismatch, untrusted peer, and concurrent restart identity checks pass independent tests. |
| A07 | Sustained streams remain within queue budgets without losing decisions or final items. |
| A08 | Disconnect races produce reconciled state or typed uncertainty, never duplicate prompts. |
| A09 | Local, daemon-host, and runner path domains remain distinct under remote execution. |
| A10 | Restore and close preserve drafts without recreating side effects or retiring the daemon. |

Verification details belong in the native verification document.
Passing wire tests alone does not establish desktop release readiness.
