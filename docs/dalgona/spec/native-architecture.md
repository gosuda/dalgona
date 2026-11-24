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
| Remote transport | `client.Remotes` owns the remote table and box bridge. `remote.Binder` keeps thread runner binding. | Add box bridges, daemon relays, endpoint management, and cloud operations to the lifecycle owner. |
| Desktop | `cmd/dalgona-gui` builds with the `gui` tag. It shows the shell regions and asks before a close drops pending decisions. It has no daemon connection yet. | Connect the window through the lifecycle owner and complete every required component. |
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

Candidate B shares endpoint lifecycle code and connects directly to trusted local IPC, a box bridge, or a daemon relay.
It preserves separate daemons and simultaneous endpoint sessions while removing the proxy process from desktop operation.
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
| `EndpointID` | `client` | Stable client-scoped identity derived from a canonical local home or configured remote name. |
| `Endpoint` | `client` | One local home or configured remote, its display label, transport kind, and configuration revision. |
| `EndpointSet` | `client` | Up to 32 configured endpoints and eight live transports, with independent session admission. |
| `BackendArtifact` | `client` | Absolute executable path, verified build identifier, and selected product. |
| `EndpointSession` | `client` | One endpoint, connection generation, phase, retry ladder, subscriptions, budgets, and receipt indexes. |
| `DaemonIdentity` | `rpc` | Observed daemon instance, backend build, protocol revision, and platform. |
| `BuildMismatchError` | `client` | Running identity and selected backend identity remain available as typed fields. |
| `UnknownOutcomeError` | `rpc` | A mutation was dispatched without a conclusive response. |
| `SessionState` | `gui` | Endpoint-scoped thread projections, pending decisions, drafts, and connection state. |
| `ThreadProjection` | `gui` | One endpoint, stream identity, applied ordinal, active branch revision, and authoritative items. |
| `DraftState` | `gui` | One window, endpoint, and draft identity, with an optional target thread. |
| `DocumentState` | `gui/document` | Endpoint, document version, selection, folds, and source ranges. |
| `TerminalView` | `gui/terminal` | Endpoint, terminal identity, checkpoint, output and resize cursors, attachment, and writer lease. |
| `WindowState` | `gui` | Gio widget state, focus, layout, and accessibility targets for one window. |

The local `EndpointID` MUST contain the canonical product home path.
A box, daemon, SSH, or paired-device remote `EndpointID` MUST contain the configured remote name.
The identity type MUST distinguish local, box, daemon, SSH, and paired-device sources.
Display names MUST NOT serve as cache keys or authority tokens.
Daemon instance identity MUST change after every daemon restart.
A remote daemon home MUST have a persistent machine identity separate from its process identity, endpoint name, and transport address.
An authenticated reconnect MUST verify the bound machine identity before workspace operations.
A changed identity MUST require an explicit new binding.

The same thread identifier on two endpoints MUST identify two distinct threads.
Every client key naming a thread, item, request, path, workspace, terminal, attachment, or draft MUST include `EndpointID`.
Paths MUST NOT convert between endpoint domains.
An offline remote MUST NOT fall back to the local file system.

Drafts and preferences belong to the GUI. Credentials, provider sessions, and workspace authority belong to their backend owners.
The GUI MUST NOT persist bearer credentials, OAuth codes, terminal input, or approval answers in its restore state.
Tokens MUST NOT enter GUI restore state or logs.
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

`client` is justified by one invariant: connection trust and backend identity remain consistent for each independent endpoint generation.
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

`EndpointSet` exposes `Connect`, `Reconnect`, `Restart`, and `Close` for its `EndpointSession` values.
Every lifecycle request MUST contain an explicit target `EndpointID`.
The set MUST resolve the matching session before it acts.
`Connect` may launch a local daemon before its first successful connection.
`Reconnect` only dials the target endpoint.
`Restart` requires an explicit user action and the identity being replaced.
`Close` releases only the target client's transport and never retires the shared daemon.
Selecting a thread MUST NOT supply an implicit lifecycle target.
Every lifecycle operation MUST use its explicit target because no global endpoint context exists.

`EndpointSet` exposes `Endpoints`, `SaveRemote`, and its connection operations.
`Endpoints` returns every configured endpoint with its ID, label, kind, and phase.
`EndpointSet.SaveRemote` applies the 33-endpoint admission rule, then delegates configuration storage to `client.Remotes.SaveRemote`.
`client.Remotes` owns remote configuration storage, validation, cloud operations, and the box bridge.
`CloudUp`, `CloudPull`, and `CloudDown` remain client-side `Remotes` operations.
The other management operations act on client-owned configuration and their explicitly named remote target.
They MUST NOT execute against whichever daemon currently occupies a conversation tab.
Their typed requests carry operation identifiers and explicit target identities.

`CloudDown` compares its confirmed target with the saved remote before destruction.
It clears the saved box only if the remote still identifies the destroyed box.
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

Each session MUST expose one phase: `idle`, `connecting`, `connected`, `reconnecting`, `offline`, `incompatible`, or `failed`.
`idle` means no live transport and no failure.
An unused or idle-evicted endpoint MUST use `idle`.
The `idle` phase MUST NOT appear as a connection loss.
A session connects on first use from `idle`.
Connect MUST initialize with `dalgonaNative` and validate `dalgona/capabilities/read` before entering `connected`.
Each attempt has a 30-second deadline.
Only successful initialization and required capability validation permit `connected`.
An untrusted peer or refused token MUST use `failed` without automatic retries.
An invalid `token_file`, `url`, `tls_ca_file`, or certificate verification MUST also use `failed` without automatic retries.
An unsupported endpoint MUST use `incompatible` without automatic retries.
This includes build mismatch, missing native negotiation, missing capability methods, and an `incompatible` error kind.
An exhausted retry ladder or explicit Disconnect MUST use `offline`.
Each phase change MUST affect only its owning session.

A build mismatch MUST show the running and selected backend builds.
The GUI MUST offer an explicit restart action with the affected endpoint and active-work consequences.
It MUST NOT retire a daemon during automatic reconnect or because a window opened.
Remote restart requires a supported remote lifecycle action and its own confirmation.

Missing native capabilities produce `incompatible` with a backend update action.
The application MUST NOT display apparently complete controls backed by unavailable methods.
Restart MUST recheck identity immediately before retirement to avoid retiring a replacement daemon.

## A07. Connection ownership and backpressure

One application process owns an `EndpointSet` with at most 32 configured endpoints.
The set owns one `EndpointSession` per configured endpoint, including sessions without live transports.
An endpoint connects on first use from `idle`.
Each session MUST have at most one live transport in `connecting`, `connected`, or `reconnecting`.
Those three phases count against the transport cap.
The application MUST hold at most eight live transports.
Windows share their endpoint sessions through the application owner.
A window MUST NOT open an additional daemon connection merely to display the same thread.

When a ninth transport is required, the set MUST close the least-recently-used eligible connected session.
An eligible session MUST have no attachments and no calls in flight.
The evicted endpoint MUST enter `idle` without appearing disconnected or failed.
If no session is eligible, the connection request MUST report capacity without closing another session.
Selecting a thread MUST NOT disconnect or reconnect any endpoint.
The application counts interested views by endpoint and thread identity.
One shared projection and history handle serve every interested view of that thread on that endpoint.
Each view retains its own scroll position, selection, and pagination cursor.

A local endpoint MUST use the local socket from `client.DialHome` at its canonical product home.
A box remote MUST use `client.Remotes.Bridge` over the sandbox service exec transport.
A daemon remote MUST join `/rpc` to its configured `https` or `wss` base URL.
Both schemes MUST use the same TLS-protected `dalgona web` relay transport.
The connection MUST require TLS 1.3 as its minimum version and MUST NOT follow redirects.
Trust MUST come from the system root pool plus `tls_ca_file` when set.
The client MUST reread `token_file` at every dial.
It MUST send the bearer token in the `Authorization` header and MUST NOT place it in the URL.
An SSH remote MUST use system OpenSSH with verified host keys, batch authentication, no terminal allocation, and no agent forwarding.
The client MUST NOT answer host-key prompts or bypass host-key verification.
SSH onboarding MUST probe the target platform before installation.
It MUST select a matching trusted local CLI artifact, transfer it into a private stage, verify its digest, and publish it atomically.
A missing artifact or unsupported target MUST fail before installation.
Reconnect MUST NOT install, replace, or retire a shared daemon.
A successful SSH transport MUST own its child process until transport close.
The connection-attempt context MUST NOT control the lifetime of an established transport.
Disconnect MUST close the client's SSH transport without stopping the shared daemon.

Device pairing MUST grant full access to the daemon account.
It MUST NOT claim an execution boundary between a paired device and that account.
The consent surface MUST state that shell and file operations can create other access paths.
Revocation MUST reject the device credential and close its registered connections.
Revocation MUST NOT claim to reverse prior effects or remove access created through other means.

Pairing MUST bind a canonical TLS origin and persistent machine identity before transmitting a grant or device credential.
TLS connections MUST retain certificate validation, TLS 1.3, and the no-redirect rule.
The trusted pairing handoff MUST supply the relay-specific TLS public-key pin.
Every secret-bearing connection MUST verify that pin and the expected machine identity.
A changed pin MUST require explicit rebinding.
Pairing grants MUST expire after five minutes and MUST admit only one successful exchange.
The daemon MUST discard unused grants on restart.
The daemon MUST store device credential digests, not their plaintext values.
The client MUST store its credential atomically in an owner-only file before publishing the endpoint.
It MUST reread that credential for every connection attempt.
Device credentials and pairing grants MUST NOT enter URLs, process arguments, logs, or frontend restore state.
Admission and revocation MUST share one authority so a revoked credential cannot race into a new connection.
An uncertain credential-store write MUST fail closed.
Pairing MUST NOT publish a network listener without an explicit owner action.
A machine frame with a version other than 1 MUST fail with `unsupported daemon machine protocol; update the daemon` without starting an RPC session.

The TUI MUST use the shared endpoint lifecycle owner rather than add a second connection manager.
Every deferred command, result, notification, and approval MUST carry its originating endpoint and connection generation.
Switching the visible machine MUST NOT redirect pending operations.
Connection loss MUST preserve drafts and invalidate decisions from the lost generation.
Reconnect MUST NOT replay a mutation whose outcome is unknown.
The endpoint set MUST select an explicit consumer protocol profile.
The native desktop profile MUST retain its required native capability negotiation.
The TUI profile MUST retain the existing Codex wire format without claiming unsupported native extensions.
The chosen profile MUST apply to initial and newly added endpoints.
Both profiles MUST preserve endpoint generations and unknown mutation outcomes.
Neither profile MAY silently fall back after a protocol refusal.

The client MUST NOT add a cloud relay or automatic load balancing.

The connection reader validates framing and transfers owned immutable messages to the session worker.
The session worker alone mutates protocol state.
The Gio event loop alone mutates widgets, focus, layout operations, and image resources.
Workers publish immutable view snapshots and call window invalidation.
They MUST NOT call widget layout or retain a frame's Gio operations.

| Queue or cache | Required bound | Full behavior |
| --- | --- | --- |
| Endpoint transports | Eight per application across `connecting`, `connected`, and `reconnecting`. | Evict the least-recently-used eligible connected session, else refuse admission. |
| RPC dispatch | Existing 256 messages plus 16 MiB per endpoint. | Close only the affected connection and reconcile. |
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

Each transport attempt MUST increment its endpoint's connection generation.
A call on a non-current link MUST fail before dispatch.
Each endpoint connection generation owns its calls, subscriptions, pending decisions, and caches.
Responses from a previous generation MUST be ignored after their outcome is recorded.
A generation change MUST clear only that endpoint's credentials in flight, request state, workspace handles, and terminal leases.
Cached and restored data from every other endpoint MUST remain unchanged.
Drafts remain partitioned by their original endpoint.

For a retryable failure, Connect MUST make the first attempt immediately.
It MUST then make one attempt after each delay in the one-second, two-second, and four-second ladder.
After the last ladder attempt fails, the session MUST enter `offline` and require explicit Connect.
Transport-loss recovery MUST use the same three delayed recovery attempts.
After the final recovery attempt fails, the session MUST enter `offline` and require explicit Connect.
Each retry attempt has a 30-second deadline and displays `reconnecting`.
Send for that endpoint MUST remain blocked until explicit Connect succeeds.
Other endpoints MUST retain their available send and approval actions.
Automatic retry MUST NOT restart a daemon.

An explicit Disconnect MUST enter `offline` immediately and block send until explicit Connect succeeds.
Idle eviction MUST enter `idle` instead and MUST NOT invoke this loss path.

Reconciliation initializes only the affected connection, validates identity, opens fresh snapshots, and replaces its thread projections atomically.
Unsent drafts remain editable, subject to the owning endpoint's send gate.
Dispatched prompts MUST NOT be sent again automatically.
The GUI resolves uncertain operations through recorded operation identifiers on the same endpoint where supported.
Legacy mutation uncertainty requires inspection and explicit user action.

Cancelling a client context cancels waiting, not an acknowledged side effect.
Turn interruption MUST use `turn/interrupt`.
Terminal termination MUST use `dalgona/terminal/close`.
Terminal attachment release MUST use `dalgona/terminal/detach` without implying process termination.
Authentication cancellation MUST use its transaction cancellation method.

## A09. Authorization and path domains

Every displayed path MUST carry its `EndpointID` and domain: GUI local, daemon host, or thread runner.
The GUI MUST never translate a remote path through local filesystem operations.
It MUST NOT use the local file system as a fallback for an offline remote.
Remote `/workspace` paths remain POSIX paths even on a Windows GUI host.
File selection sends bytes when the backend cannot access the selected local path.

Workspace handles bind the endpoint, thread, runner identity, root, rights, and binding revision.
The daemon validates these fields before every operation.
Runner rebinding invalidates old handles before any filesystem mutation.
User filesystem actions never inherit an agent approval silently.

The shared client owner MUST key every subscription, workspace, terminal, request, and path interest table by `EndpointID`.
It MUST count workspace interests per endpoint across windows, file views, completion, commands, and terminal attachments.
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
Every open thread reference MUST contain its `EndpointID` and thread identifier.
Restore MUST recreate every saved endpoint lane without merging equal thread identifiers.
Daemon bearer tokens MUST NOT appear in restore data.
Persist restore data atomically within the GUI state directory with private permissions.
Restore data MUST carry a schema version and bounded collection sizes.
The manifest is limited to 1 MiB, 32 endpoint references, 32 thread references, and 64 draft references.
Draft bodies live in separate private files, with 1 MiB per draft and 16 MiB combined.
Draft persistence keys include the stable window identifier, `EndpointID`, and draft identifier.
Restoring two windows cannot overwrite either window's draft for the same endpoint and thread.
Draft attachment blobs use a separate 64 MiB combined budget and content digests.
A source file can remain referenced only while its identity and digest still match.

At a persistence limit, editing continues in memory with an explicit unsaved indicator.
The GUI offers export, removal of another saved draft, or explicit discard.
Window closure requires a decision before losing unsaved content.
It MUST NOT truncate a prompt, discard attachments, or evict a draft silently.
Large pasted attachments retain their normal send limits and receive a visible storage error if persistence lacks capacity.

The native client writes the manifest `restore.json` in the GUI state directory with permission 0600.
Version 1 contains `version`, `endpoints`, `windows`, and `drafts`.
Each window has an `id` and an ordered list of `{endpoint, thread}` views.
Each draft reference has `endpoint`, `window`, `thread`, and `file`.
The file name is the first 16 bytes of the SHA-256 digest of the draft key, in hexadecimal, with the suffix `.draft`.
A draft file contains its key, its text, and its attachments.
The client writes each file to a temporary file, syncs it, and renames it.
It writes the draft files before the manifest. It removes unreferenced draft files after the manifest.
Restore refuses a manifest with an unknown version or more references than the limits, and shows one notice.
Restore skips a draft file that is damaged, too large, or names a different key, and shows one notice for each skipped draft.
A draft that does not fit a limit stays in memory, and `View.Unsaved` reports it for the unsaved indicator.

Restore MAY establish live transports only within the eight-transport cap and the ordinary first-use rule.
Every other restored endpoint MUST begin in `idle`, without a loss indication.
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

Application exit cancels owned authentication flows and closes explicitly owned terminals on every affected endpoint after the required user decision.
Stop retains its reader until final output is collected, the user releases the view, or output retention expires.
Expiry releases local attachment and workspace interests while keeping collected output and the visible loss boundary.
The terminal owner detaches before releasing its final workspace interest.
Transient disconnection of one endpoint uses terminal lease recovery instead of immediate process termination on any endpoint.
Every shared daemon remains available to TUI, web, app-server, and other clients.

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
| A03 | Equal identifiers on two endpoints cannot mix responses, secrets, filesystem actions, panel sessions, or drafts. |
| A04 | Headless products build without Gio or desktop native libraries. |
| A05 | Every lifecycle operation resolves its explicit target, and all clients exercise one lifecycle implementation. |
| A06 | Per-endpoint phases and backend identity checks remain independent under concurrent startup and restart. |
| A07 | Eight live transports support concurrent streams, and the ninth admission follows idle eviction without losing attachments. |
| A08 | One endpoint's disconnect leaves other streams live and produces reconciled state or typed uncertainty without duplicate prompts. |
| A09 | Interest tables, local paths, daemon-host paths, and runner paths remain scoped to their owning endpoint. |
| A10 | Restore recreates all endpoint lanes within transport bounds without exposing tokens or recreating side effects. |

Verification details belong in the native verification document.
Passing wire tests alone does not establish desktop release readiness.
