# Native desktop client

## Status and scope

This suite defines the complete Go and Gio desktop client for dalgona.
It is a required implementation contract, not evidence that the desktop client already exists.
The specification change adds no executable and certifies no desktop acceptance result.

The source baseline is commit `2c036c60b379e7718255a23ebd2c5079d76ec50c`.
The design uses the existing [web contract](web.md) and [visual system](../../../DESIGN.md).
The shared [input text contract](../../dal/spec/input-text.md) defines Unicode behavior across GUI, TUI, web, and daemon.
Its conformance requirements cover every native editable field and remain implementation work.
The [native client decision](../decisions/native-client.md) records the selected architecture and its rejected alternatives.

N01. The client MUST use Gio for application layout, input, and rendering.
It MUST NOT embed Electron, Chromium, a WebView, a browser DOM, or an HTML layout engine.
An external system browser MAY complete an explicit OAuth action.
That browser MUST NOT host any desktop application screen.

N02. The release MUST include every required component and integration in this suite.
Implementation order MUST NOT become permission to omit a required component.
Source-only diagrams, inert controls, fake terminal output, and a CLI-only login escape path MUST NOT count as completed native features.

N03. The application MUST retain the web client's restrained composition.
The main window holds navigation, one active thread, and its request and composer dock.
Additional tools open through commands, dialogs, or a temporary inspection view.
The application MUST NOT add a permanent dashboard to expose every capability at once.

N04. The client MUST support Linux, macOS, and Windows under the [platform contract](native-platform.md).
Native input, window integration, accessibility, and packaging are required work on each supported platform.
An upstream toolkit gap MUST have an owned implementation and acceptance test before release.

N05. The daemon MUST retain execution, journal, policy, credential, and runner authority.
The desktop MUST NOT become another agent host or journal writer.
The [architecture](native-architecture.md) defines the shared Go connection lifecycle and the single state owner.

## Contract map

Each document owns a distinct decision.
References to another document inherit its requirements without copying its contract.

|Document|Owns|Requirement prefix|
|---|---|---|
|[Architecture](native-architecture.md)|Packages, types, lifecycle, concurrency, subscriptions, and reconnect.|A|
|[Protocol](native-protocol.md)|Existing API reuse and required extensions with bounds and errors.|R|
|[Design](native-design.md)|Web design mapping, native tokens, layout, focus, and keyboard behavior.|V|
|[Components](native-components.md)|Every web component and the additional desktop workflows.|C|
|[Documents](native-documents.md)|Selectable documents, diagrams, math, images, diffs, and terminal rendering.|D|
|[Platforms](native-platform.md)|Native adapters, accessibility, build isolation, packaging, and installation.|P|
|[Verification](native-verification.md)|Behavioral evidence, platform coverage, performance targets, and implementation order.|T|
|[Input text](../../dal/spec/input-text.md)|Shared text preservation, segmentation, composition, offset units, field policies, and fixtures.|I|

N06. A conflicting interpretation MUST follow the document that owns that decision.
Existing shared contracts retain authority over their existing methods and storage formats.
New desktop methods apply only when the daemon advertises the required capability.
The GUI MUST report an incompatible daemon with an actionable explanation.
It MUST NOT silently remove a required feature or guess a compatible wire format.

## Complete product coverage

The component contract maps all 43 Svelte components present at the source baseline.
It also specifies the native tasks that the current web UI does not implement.
Those additions are explicit requirements, not claims of existing web parity.

|User task|Required result|Contract|
|---|---|---|
|Open a workspace|Select a local folder or a configured remote endpoint with clear ownership.|[Connection lifecycle](native-architecture.md), [platforms](native-platform.md)|
|Find a conversation|Page, search, resume, rename, archive, and restore threads.|[Components](native-components.md)|
|Continue work|Send, steer, queue, edit a draft, attach content, and interrupt.|[Components](native-components.md)|
|Read a conversation|Select and copy text across styled and unloaded document blocks.|[Documents](native-documents.md)|
|Read rich output|Render Markdown, code, Mermaid, D2, nomnoml, SVG, math, images, and unified diffs.|[Documents](native-documents.md)|
|Inspect execution|Read complete command and tool output without loading it all into memory.|[Documents](native-documents.md), [protocol](native-protocol.md)|
|Make a decision|Approve or refuse an action and answer a question once across connected clients.|[Components](native-components.md), [protocol](native-protocol.md)|
|Navigate delegated work|Open child threads, inspect plans, and follow the active branch.|[Components](native-components.md)|
|Change conversation history|Compact, fork, and rewind with an accurate statement of file effects.|[Components](native-components.md)|
|Review changes|Choose each supported review target, inspect diffs, and read advisor results.|[Components](native-components.md)|
|Configure inference|Select models, effort, providers, credentials, and supported settings.|[Components](native-components.md), [protocol](native-protocol.md)|
|Use remote execution|Select, create, inspect, pull, and remove configured boxes with explicit scope.|[Architecture](native-architecture.md), [components](native-components.md)|
|Use MCP|Inspect connection state and tools, authenticate, and manage supported configuration.|[Components](native-components.md), [protocol](native-protocol.md)|
|Inspect workspace files|Browse and edit the actual thread workspace with explicit conflict and authority checks.|[Protocol](native-protocol.md)|
|Use an interactive terminal|Attach a real PTY with input, resize, bounded output, and declared disconnect behavior.|[Protocol](native-protocol.md), [documents](native-documents.md)|
|Recover from an error|Keep drafts, identify uncertain outcomes, reconnect, and reconcile authoritative state.|[Architecture](native-architecture.md)|
|Diagnose or report a failure|Run supported diagnostics and preview an explicitly consented, masked report.|[Components](native-components.md)|
|Use assistive technology|Navigate, read, edit, select, and act through native accessibility APIs.|[Platforms](native-platform.md)|

## Current implementation and required changes

This table identifies gaps at the source baseline.
It does not weaken the complete release requirement.

|Area|Existing behavior|Required implementation|
|---|---|---|
|Connection|The TUI uses Go RPC. CLI packages own daemon startup and remote selection.|Extract one shared lifecycle and migrate its consumers.|
|RPC package|Client transport and server handlers share one package.|Separate client-consumable wire code from server implementation.|
|History|Existing turn pagination still reads the complete journal.|Add bounded snapshot paging with an explicit live-event boundary.|
|Subscriptions|The upstream schema has `thread/unsubscribe`, but the server does not implement it.|Implement detach without archiving or stopping another client.|
|Stored images|User items contain blob references without retrieval bytes.|Add item-scoped retrieval and validate image ownership.|
|Workspace files|Existing `fs/*` addresses the daemon host.|Add explicit thread-runner file operations.|
|Terminal|`command/exec` rejects PTY, input streaming, and terminal size.|Implement real terminal lifecycle and transport.|
|Authentication|Several provider and MCP flows require terminal interaction.|Expose the complete native authentication workflow.|
|Input text|Existing adapters mix cleanup, quota units, and source display handling.|Implement the shared preservation and Unicode conformance contract across clients.|
|Figures|Web renderers use browser display APIs. TUI renderers produce terminal output.|Implement bounded native figure rendering.|
|Math|The current web renderer has no math component.|Implement the declared native math grammar and layout.|
|Accessibility|Gio semantic operations do not supply all desktop bridges.|Implement and test each desktop bridge.|

The [source map](#source-map) anchors these observations.
The protocol specification MUST label every new method as proposed until implementation lands.

## User-visible boundaries

N07. The application MUST distinguish the desktop host, daemon host, and thread runner.
A path MUST retain its owning endpoint and workspace.
A remote path MUST NOT become a local file URL or a local command argument through implicit conversion.

N08. A thread rewind changes conversation history under the existing rewind contract.
The UI MUST NOT describe rewind as a filesystem rollback.
The UI MUST preserve the distinction between an approval mode and a sandbox mode.
The local harness sandbox MUST NOT be described as isolation from hostile code.

N09. A reconnect MUST NOT replay a user mutation whose outcome is unknown.
Draft preservation MUST NOT imply that a previously sent turn failed to start.
The client MUST reconcile daemon state before offering a deliberate retry.

N10. Closing the desktop MUST follow the existing last-subscriber rules for approvals and questions.
The close flow MUST explain affected pending decisions.
It MUST NOT imply that keeping the daemon alive preserves every unanswered request.

## Implementation contract

N11. New runtime code MUST remain in the existing Go module.
The desktop command is `cmd/dalgona-gui`.
Gio code belongs to `internal/dalgona/gui` and its justified implementation packages.
Shared connection policy belongs to `internal/dalgona/client`.
The package dependency rules and their migration are defined in [architecture](native-architecture.md).

N12. Headless binaries and remote box builds MUST retain their current graphics-free build path.
Desktop dependencies MUST have dedicated build and verification targets.
Dependency pins, source notices, font licenses, and renderer assets MUST ship with the implementation that introduces them.

N13. Every required workflow MUST specify loading, empty, success, cancellation, refusal, and failure behavior where those states apply.
An implementation MUST NOT leave a visible action without its complete behavior.
Unsupported external content MUST have a visible diagnostic and a safe inspection path.
Supported content MUST render successfully within its declared limits.

N14. Completion requires every applicable row in [verification](native-verification.md).
Passing a headless test MUST NOT be reported as a native input or screen-reader test.
The documentation PR itself requires documentation checks and source review.
It MUST NOT claim the future runtime gates have passed.

## Source map

The following sources were reviewed for this specification.

|Source|Evidence used|
|---|---|
|[Visual system](../../../DESIGN.md)|Palette, type, spacing, controls, state words, motion, icons, and marks.|
|[Web contract](web.md)|Existing user workflows, focus, editor, content, requests, and panels.|
|[Web components](../../../web/src/lib/components)|The complete existing component inventory.|
|[Client connection](../../../internal/dal/rpc/conn.go)|Go transport, requests, cancellation, and bounded client dispatch.|
|[Server registration](../../../internal/dal/rpcserver/server.go)|Implemented methods and host handler registration.|
|[Protocol](../../dal/spec/protocol.md)|Wire framing, limits, ordering, images, and command behavior.|
|[Daemon](daemon.md)|Local startup, identity, peer checks, and service ownership.|
|[Remote runners](runners.md)|Local host, thread-box, and daemon-in-box distinctions.|
|[OAuth](../../dal/spec/oauth.md)|Provider flows, credential authority, and existing terminal-only paths.|
|[Approvals](../../dal/spec/approvals.md), [questions](../../dal/spec/ask.md)|First-response behavior and last-subscriber consequences.|
|[Gio documentation](https://gioui.org/)|Native application architecture and platform setup.|
|[Gio Context7 index](https://context7.com/git_git_sr_ht/~eliasnaur_gio)|API discovery and examples checked against current package documentation.|

## Gio API authority

N15. The implementation MUST pin compatible Gio module versions before its first runtime change.
API signatures MUST come from that pin's package documentation and source.
Architecture articles and Context7 examples explain concepts but MUST NOT override the pinned API.

The reviewed APIs use `Window.Event`, `app.FrameEvent`, `app.NewContext`, `input.Source`, and `Window.Invalidate`.
Widget state and operation lifetimes follow the [native architecture](native-architecture.md).
Examples using an obsolete event channel, `Queue`, `text.Cache`, or assignable `Selectable.Text` MUST NOT become implementation requirements.

Reference pages include [window](https://gioui.org/doc/architecture/window), [drawing](https://gioui.org/doc/architecture/drawing), and [input](https://gioui.org/doc/architecture/input).
The [widget](https://gioui.org/doc/architecture/widget), [layout](https://gioui.org/doc/architecture/layout), and [text](https://gioui.org/doc/architecture/text) pages explain ownership and layout.
Current signatures are available in [app](https://pkg.go.dev/gioui.org/app), [widget](https://pkg.go.dev/gioui.org/widget), and [text](https://pkg.go.dev/gioui.org/text).
