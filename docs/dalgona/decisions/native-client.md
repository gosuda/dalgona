# Native Gio client

## Invariant

Dalgona has one Go native desktop client built with Gio.
The desktop renders its own controls and documents without a browser engine.
The existing daemon retains execution, journal, policy, credential, and runner authority.
The desktop is a client of that authority.

The native UI follows [DESIGN.md](../../../DESIGN.md) and the existing [web behavior](../spec/web.md).
Its complete contract starts at [native.md](../spec/native.md).
Minimal design means few simultaneous controls, not missing components or inaccessible actions.

## Scope

This decision authorizes the package boundaries required by the native specification.
The wire and server split and the lifecycle package exist. The GUI packages do not exist yet.

The runtime remains in one Go module.
The desktop entry is `cmd/dalgona-gui`.
Presentation code lives in `internal/dalgona/gui`.
Only native adapters and rendering code import Gio or platform GUI libraries.

Shared connection policy lives in `internal/dalgona/client`.
Local and remote transport code must expose the same security and lifecycle rules to all clients.
The CLI, web relay, app-server, and desktop migrate to that policy.
The migration removes the displaced connection implementations in the same change.
`client.Session` owns startup, reconnect, restart, and close for one endpoint.
`client.Remotes` owns the `[remotes]` table and the remote bridge.
`internal/dalgona/daemon` imports `client` for the home, socket, and peer checks it shares with dialing.
`internal/dalgona/remote` imports `client.Remotes` for cloud up, down, and pull.
`client` imports neither of them.

Client-consumable RPC framing and types remain separate from server handlers.
The extraction must account for initialization roots and transitive imports.
Moving one wrapper does not prove that a GUI binary has no server dependencies.
The implementation must inspect the resulting import graph and report measured binary sizes separately.

The new boundary must not introduce a second agent protocol, a generic service framework, or forwarding-only interfaces.
The client package hides endpoint selection, backend identity, secure dialing, and process lifecycle.
The GUI owns display state and sends explicit user intents.
The server owns durable changes.

## Architecture choice

Two complete process arrangements were compared against the current source.

|Criterion|Bundled app-server over stdio|Shared Go lifecycle with direct IPC|
|---|---|---|
|Initial integration|Reuses the existing bridge without moving callers.|Requires one extraction and caller migration.|
|Daemon authority|Preserves the daemon boundary.|Preserves the same boundary.|
|Lifecycle diagnostics|A helper exit needs a structured diagnostic channel.|The caller receives typed lifecycle errors directly.|
|Runtime ownership|The GUI owns a bridge child and its pipes.|The GUI owns a connection and explicit startup policy.|
|Policy reuse|Existing command code remains the policy owner.|All clients share one importable policy owner.|
|Remote behavior|Uses the bridge's existing remote selection.|Moves that same remote selection into the shared package.|

The selected arrangement is shared Go lifecycle code with direct IPC.
The complete implementation request justifies extracting the existing policy once.
This choice avoids adding a permanent proxy process solely for the desktop.
It also gives the GUI typed restart, trust, compatibility, and connection failures.

The alternative contributes a constraint rather than an extra mode.
The existing app-server behavior remains a compatibility consumer of the extracted policy.
The desktop does not offer a setting to select between both arrangements.

The daemon remains a separate process in both arrangements.
No measured transport or rendering speedup is claimed by this decision.

## Interface and data ownership

The package and type sketch lives in [native-architecture.md](../spec/native-architecture.md).
Its caller-facing operations complete a connection task without exposing its internal stages.
Backend executable identity is explicit and never inferred from the GUI executable's bytes.
The session worker owns protocol projections and applies ordered typed events.
The Gio event loop owns widget, focus, and layout state.
Workers publish immutable snapshots tagged with endpoint, connection, and document identities.

The document model owns selection and stable text positions across virtualized blocks.
Gio widget instances are not the authority for a whole document's selection.
Native accessibility adapters read the same semantic document model.
They send actions back through the GUI owner.

Untrusted figure work runs in a bounded, killable renderer process.
The [document contract](../spec/native-documents.md) defines its accepted input and output.
The worker API exposes no browser, UI scripts, network access, or provider credentials.
The desktop does not run model-supplied code to render a diagram.
Worker limits contain failures and resource use. They are not an operating-system security boundary.

## Enforcement

The implementation must update `.golangci.yml` in the same change that introduces each new import boundary.
The `lifecycle-client` rule keeps `client` free of the agent core, journal, tools, server handlers, the daemon runtime, and thread runner binding.
Core packages must continue to reject presentation imports.
The GUI must not import journal or agent internals to bypass the wire.
Only the shared lifecycle package may select and start the backend.

The existing headless build and remote box build remain free of GUI dependencies.
A dedicated desktop build target must compile all supported targets.
Desktop CI must exercise the tagged files, not merely exclude them from the ordinary checks.

The [component inventory](../spec/native-components.md) covers every existing web component.
The [verification contract](../spec/native-verification.md) binds those components to observable tests.
No required component may ship as an inert placeholder or an unimplemented CLI handoff.

The platform work includes desktop accessibility bridges, native folder selection, image clipboard, and Korean IME verification.
Their absence upstream does not remove them from the release contract.

The implementation must preserve existing Codex payloads and request identities.
New functions use the versioned capabilities in [native-protocol.md](../spec/native-protocol.md).
Journal framing, recovery, approval order, and secret masking retain their existing authority.

## Tradeoffs accepted

- Custom native controls require application-owned interaction tests.
- Shared lifecycle extraction changes several current consumers before it reduces duplicated policy.
- A separate daemon requires reconnect and uncertain-outcome handling.
- A killable renderer adds one bounded worker process to protect input responsiveness.
- Native accessibility requires platform-specific code beyond Gio's drawing operations.
- A complete native release needs platform evidence that this documentation change cannot provide.

## Rejected alternatives

- Rust and GPUI add another client implementation language without a required capability that justifies that boundary here.
- Electron and WebView frameworks violate the requested native rendering constraint.
- Embedding the agent host in the GUI couples execution and journal lifetime to a window.
- A permanent desktop app-server proxy keeps connection policy in command code and requires another diagnostic protocol.
- Importing the Bubble Tea model carries terminal layout and command types into the GUI.
- A general widget framework inside dalgona adds interfaces without a separate product that needs them.
- Source-only diagram rendering leaves an existing web component incomplete.
- A read-only command log labeled Terminal hides the missing PTY contract.
- Accessibility labels without OS bridges leave desktop screen readers unable to act on the UI.
