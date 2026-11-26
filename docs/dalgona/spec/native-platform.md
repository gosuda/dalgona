# Native platform integration

## Status and scope

This document specifies required desktop implementation work.
It does not claim that Gio already supplies every capability or that platform acceptance has passed.
All platform contracts below are required for the complete native release.
The [implementation status](#implementation-status) section records the owned platform work and each missing behavior.

Read this with [the client specification](native.md), [documents](native-documents.md), [protocol](native-protocol.md), and [verification](native-verification.md).
The [design specification](native-design.md) owns visual tokens.
The [native client decision](../decisions/native-client.md) owns the language and process choices.
The [shared input contract](../../dal/spec/input-text.md) owns accepted text, Unicode boundaries, field limits, and validation across all clients and the daemon.

## P01. Own native integration explicitly

The executable MUST be `cmd/dalgona-gui`.
It MUST use the shared lifecycle package `internal/dalgona/client` and connect to the separate daemon.
Platform adapters MUST live under `internal/dalgona/gui/platform`.
They MUST hide operating-system handles, callback rules, and data formats from document and component code.

The implementation MUST use Go for application state and Gio for the interface.
Small native bridges MAY use Objective-C or system calling conventions where the operating system requires them.
No platform MAY load an embedded browser, WebView, WebKit, Chromium, or Electron runtime.

| Capability | Existing foundation | Required owned work |
|---|---|---|
| Window and GPU rendering | Gio `app.Window` | Lifecycle, scaling, restoration, diagnostics, and adapter attachment. |
| Text entry | Gio `widget.Editor` | Composition-aware actions and platform acceptance fixes. |
| Text clipboard | Gio clipboard commands | Multiple formats, image clipboard, file references, and clipboard ownership. |
| File open and save | Native dialog APIs and the XDG portal | Folder selection, parent ownership, cancellation, and complete platform behavior. |
| Desktop accessibility | Gio semantic operations | Complete AppKit, UI Automation, and AT-SPI bridges. |
| Menus and notifications | Platform APIs | Native command routing, permissions, activation, and lifecycle. |
| Interactive terminal | Native PTY and ConPTY APIs | The daemon protocol and runner implementation specified separately. |

The implementation MUST not report a capability as complete merely because a dependency exposes related types.
The [verification specification](native-verification.md) defines release evidence.

## P02. Use the current Gio event lifecycle

The main goroutine MUST call `app.Main` as required by Gio.
Each window MUST have one owner for widget state, focus, selections, and frame operations.
Its loop MUST use `Window.Event`, `app.FrameEvent`, `app.NewContext`, and the frame submission callback.
The implementation MUST not copy removed `NewWindow`, `StageEvent`, or event-channel APIs from older examples.

Workers MUST publish typed results through bounded queues.
They MAY call `Window.Invalidate` after enqueueing a result.
They MUST not mutate widget state or issue Gio drawing operations.
The [`Window.Invalidate` contract](https://pkg.go.dev/gioui.org/app#Window.Invalidate) permits concurrent use.

The client MUST request frames for input, changed state, visible animation, and platform exposure.
It MUST not run a permanent frame ticker.
Caret blinking and the shared design's live indicators MAY schedule their necessary frames.
Reduced motion and inactive windows MUST stop unnecessary scheduling.

Each frame MUST consume a bounded batch of UI and platform intents before layout.
It MUST consume the latest immutable view snapshot without replaying protocol events.
View updates MAY replace a superseded snapshot after ordered protocol state has been applied.
The client MUST not discard protocol deltas to reduce painting work.
The application session reducer MUST continue processing protocol state while windows are minimized or receive no frame events.
Frame delivery MUST not be the condition for draining transport queues.
The frame owner consumes published view snapshots rather than taking ownership of that reducer.

Gio MUST retain ownership of its GPU renderer.
The implementation MUST not enable `CustomRenderer(true)` merely to obtain native handles.
That option disables Gio's normal GPU rendering path.

## P03. Bind platform handles to their lifetime

Each valid `app.ViewEvent` MUST establish a new platform-handle generation.
Adapters MUST attach only to handles from that generation.
An invalid event MUST detach menus, accessibility providers, dialogs, drop targets, and native callbacks referencing earlier handles.
The [`ViewEvent` contract](https://pkg.go.dev/gioui.org/app#ViewEvent) explicitly invalidates previous handles.

| Backend | Handle basis | Required ownership |
|---|---|---|
| macOS | `AppKitViewEvent.View` and its layer | Attach AppKit bridges on the native event thread. Never release a Gio-owned view. |
| Windows | `Win32ViewEvent.HWND` | Attach and remove native window hooks without replacing Gio's input handling. |
| X11 | Display and window handles from `X11ViewEvent` | Preserve X11 event ownership and clipboard lifetimes. |
| Wayland | Display, surface, and toplevel from `WaylandViewEvent` | Bind protocol objects to the current connection and destroy only owned objects. |

Native callbacks MUST publish intents into the window owner.
They MUST not synchronously wait for an RPC or renderer result.
Accessibility queries MUST read immutable snapshots and avoid locks held by the frame owner.
Closing a window MUST cancel its pending operations before releasing adapter state.

## P04. Preserve window and display behavior

The initial client area MUST be 1280 by 800 logical pixels.
The minimum client area MUST be 320 by 320 logical pixels.
The client MUST use the sidebar, transcript, and breakpoint values in [native design](native-design.md).
It MUST preserve required controls when the window reaches its minimum size.

Window state MUST include restored size, maximized state, display-relative position, and selected thread.
Restoration MUST clamp a detached display's position into a visible work area.
It MUST not restore a modal dialog or a stale platform handle.
A crash recovery MUST preserve drafts through the client-state contract.

The client MUST respect fractional scale, per-monitor scale changes, work areas, native decorations, and system text preferences.
Pointer coordinates, selection geometry, IME rectangles, and accessibility bounds MUST use explicit coordinate conversions.
Moving between displays MUST not reuse cached pixels or coordinates from the old scale.

The default window MUST retain native title-bar controls and ordinary system move and resize behavior.
Closing the final window MUST not implicitly send `turn/interrupt` or retire the shared daemon.
Resulting subscriber loss MUST preserve the server's existing approval and question policies.
Closing active user terminals MUST follow P13 and the protocol's explicit close contract.

Renderer loss MUST preserve application state and reconnect the drawing resources when the backend permits recovery.
An unrecoverable graphics failure MUST show a native error and an actionable diagnostic path.
It MUST not leave an invisible process holding the window open.

## P05. Finish text input and IME integration

The composer and editable fields MUST use Gio `widget.Editor` for text composition.
Raw key handlers MUST not reconstruct IME composition from key presses.
The editor MUST retain focus, surrounding text, caret geometry, and selection while the native IME owns composition.
The platform adapter owns preedit and its lifecycle. Preedit MUST remain outside committed application text and protocol requests.
The pinned Gio editor MUST pass the shared Unicode 18.0.0 default extended-grapheme corpus under UAX #29 revision 49.
Dependency defaults MUST NOT silently select another segmentation target.

Gio's [editor API](https://pkg.go.dev/gioui.org/widget#Editor) uses the following units.
The implementation MUST verify them against its pinned Gio version.

| API | Unit |
|---|---|
| `Len`, `SelectionLen` | Rune counts. |
| `Selection`, `SetCaret` | Rune offsets, preserving selection direction. |
| `Regions(start, end, ...)` | Half-open rune range. |
| `MoveCaret(startDelta, endDelta)` | Grapheme-cluster deltas. |
| `Delete(graphemeClusters)` argument | Grapheme count with direction. A selected range counts as one requested cluster. |
| `Delete` result | Number of runes removed. |
| `Insert` result | Number of runes inserted. |

Byte offsets, UTF-16 units, runes, grapheme counts, and glyph positions MUST have explicit conversions at adapter boundaries.
Programmatic selection and replacement MUST validate scalar boundaries and enforce the shared committed-edit grapheme rules before calling Gio.
An editor API accepting rune offsets MUST NOT be treated as proof of grapheme-safe range validation.

The application MUST distinguish composition start, update, commit, and cancellation.
Enter, Escape, completion acceptance, message history, and command shortcuts MUST not steal an active composition action.
A composition commit MUST insert its text exactly once.
The same key event MUST not also send the message, choose an option, or activate a panel action.
Cancellation MUST restore committed text and selection. Rejected commits MUST retain a recoverable unsent candidate without inserting a prefix.
Each accepted composition commit MUST form one undoable transaction.
Undo history MUST stay bounded in steps and bytes.
The editor keeps the newest 1000 undo steps within 32 MiB, and it always keeps the newest step.
Gio's private widget history MUST stay bounded too.
Outside composition, the editor replaces the Gio widget after 1000 widget edits or 32 MiB of widget history.
The replacement keeps the shown text, the selection with its direction, and focus.

The adapter MUST provide a reliable composition-active signal when the pinned Gio API does not expose it publicly.
This is required bridge or upstream patch work.
Timing guesses, key-code lists, and delayed submission MUST not replace that signal.

| Platform | Required acceptance environment |
|---|---|
| Windows | Microsoft Korean, Japanese, Pinyin, and Bopomofo IMEs, plus Latin input, on every shipped Windows architecture. |
| macOS | Apple's Korean, Japanese, Pinyin, and Zhuyin input sources, plus Latin input, on every shipped macOS architecture. |
| Linux X11 | IBus and Fcitx5, each with Hangul, Japanese, and Chinese engines. Include Latin input and the selected X11 backend. |
| Linux Wayland | IBus and Fcitx5, each with Hangul, Japanese, and Chinese engines, on supported compositors through text-input-v3. |

Release evidence MUST name each exact operating system, engine, version, compositor, backend, and architecture tested.
Synthetic events MUST NOT replace tests with real candidate windows and native input engines.

Gio's [September 2026 changes](https://gioui.org/news/2026-09) add IBus support on X11 and text-input-v3 support on Wayland.
This does not prove every input engine or compositor works.
The platform owner MUST implement and test missing routes for the declared release environments.
The release MUST not silently reduce its Linux IME matrix to the easiest route.

Composition tests MUST cover Korean syllables, partial syllable deletion, replacement, cancellation, and composition at either end of a selection.
Japanese tests MUST cover candidate conversion, reconversion, and cancellation.
Chinese tests MUST cover candidate paging and selection with the declared Simplified and Traditional input sources.
They MUST cover multiline input, emoji, combining marks, surrogate pairs, and mixed-script text.
They MUST also cover candidate positioning after scrolling and between displays at different scales.
Composition in a nearly full field MUST test the exact limit, one scalar over, and a final commit that joins an adjacent grapheme.

Text insertion through accessibility MUST use the same editor operations and undo model.
It MUST not bypass draft limits or corrupt an active composition.

## P06. Ship predictable fonts

The client MUST use the faces and role assignments from [native design](native-design.md) and [DESIGN.md](../../../DESIGN.md).
Titles and labels use Bricolage Grotesque.
Body text uses Hanken Grotesk with Pretendard for Hangul.
Code and terminals use the user's system monospace family with a deterministic fallback order.

The distribution MUST bundle the required text faces and the licensed icon subset for offline operation.
It MUST not download a font when opening a conversation.
Font licenses MUST ship with the package.
The build MUST record each font's source, license, version, and digest.

Variable-font support MUST be verified against the pinned Gio font path.
If that path cannot select the required axes, the build MUST generate static instances at the exact registered weights.
The packaged client MUST not require a font-conversion tool.
Gio v0.10.3 selects no variable font axis.
`just desktop-fonts` therefore makes static Bricolage Grotesque instances at 600, 650, and 700 and Hanken Grotesk instances at 400, 600, and 700.
The Bricolage Grotesque instances use optical size 14 and width 100.
Pretendard ships unmodified static faces at 400, 600, and 700, so a 650 request uses the nearest Pretendard face.
`internal/dalgona/gui/platform/fontfaces/SHA256SUMS` records the digest of each bundled face, and `THIRD_PARTY_NOTICES.md` records each source, license, and version.
Changing the font catalog MUST invalidate shaped geometry without moving the document's logical anchor.

Fallback MUST operate on complete grapheme clusters.
It MUST preserve combining sequences, ideographic variation sequences, emoji modifiers, regional-indicator flags, and joined emoji.
Fallback lookup MUST retain variation selectors, ZWJ, and tag scalars in source text.
The client MUST support color emoji through a native glyph path or an owned bounded glyph raster adapter.
It MUST not replace readable fallback text after one missing glyph.
An unsupported cluster MUST have a stable visible fallback under the shared contract, with source-preserving copy, selection, and accessibility.
Fallback geometry MUST map to the complete original cluster without manufacturing replacement input.
The release MUST test rare supplementary CJK, variation sequences, private-use scalars, noncharacters, and mixed text with the packaged font catalog.

The platform font resolver MUST distinguish missing fonts from unreadable font files.
It MUST retain a valid built-in fallback and report corrupt user fonts in diagnostics.
The resolver MUST not repeatedly scan every system font during a frame.

Elapsed time, counts, and percentages MUST use tabular figures.
Math MUST use the separately pinned math face from [native documents](native-documents.md#d09-render-mathematical-notation).
Figure measurement and figure drawing MUST use the same font digest.

## P07. Implement clipboard and drag-and-drop formats

Gio's [`clipboard.ReadCmd`](https://pkg.go.dev/gioui.org/io/clipboard#ReadCmd) requests text.
The client MUST add native image and file-format handling.
It MUST not describe text clipboard support as complete attachment support.

| Platform | Required text and image formats |
|---|---|
| Windows | Unicode text, registered PNG, and DIB or DIBV5 images with validated dimensions and alpha. |
| macOS | Plain text, PNG, TIFF, and file URLs through `NSPasteboard`. |
| Linux X11 | UTF-8 text, PNG, JPEG, URI lists, and incremental selection transfer. |
| Linux Wayland | MIME-negotiated text, PNG, JPEG, and URI lists through the active seat's data device. |

File drops MUST preserve ordering and handle paths containing spaces, non-ASCII characters, and escaped URI characters.
Image files MUST enter the attachment validation path.
Directory drops MUST select a workspace only through the explicit workspace action.
Text drops MUST edit the focused field or create a paste chip according to component behavior.
Text transfer decoding MUST validate the declared encoding before inserting or constructing a chip.
Malformed UTF-8, unpaired UTF-16 surrogates, embedded NUL, or a failed field limit MUST reject the complete text candidate.
The adapter MUST NOT use replacement decoding, normalization, transliteration, or silent first-line truncation to accept it.
Multiline text MUST use the shared line-ending conversion. One-line fields MUST reject multiline text atomically.
An accepted paste MUST form one undoable edit and preserve the resulting scalar sequence, including final line breaks.

Clipboard reads MUST occur only for a paste or another explicit clipboard action.
The client MUST not poll or log clipboard contents.
Clipboard format priority MUST prefer an image when the user invokes Paste image.
Ordinary Paste MUST prefer text when both text and an image represent the same clipboard item.
An image-only clipboard MUST create an image attachment.

An asynchronous paste MUST retain its target field, thread, draft revision, selection, composition transaction, and window generation.
If that target no longer exists, the result MUST be discarded with a notice.
It MUST never attach to whichever thread happens to be open later.
Changed target text or selection MUST prevent stale replacement. Active preedit MUST not be overwritten by a completed transfer.
Rejected candidates MUST remain recoverable without changing committed text, selection, or undo history.

The adapter MUST bound transferred bytes before decoding and close every transfer stream.
One incoming native clipboard or drop transfer MUST contain at most 32 MiB before image conversion.
Converted image files MUST still meet the five MiB individual and ten MiB aggregate attachment limits.
An outgoing image copy MUST preserve the complete authorized image within the D10 geometry limits.
The renderer child MUST validate and encode the image before the platform publishes it.
The combined outgoing PNG and Windows CF_DIB bytes MUST NOT exceed the D11 raster-export limit of 256 MiB.
The platform MUST check each declared format length and the combined length before it allocates transfer storage.
Encoded output MUST stream into unpublished transfer storage through bounded buffers.
The GUI MUST NOT decode the image or assemble a separate full-image bitmap for clipboard publication.
A failed or cancelled preparation MUST preserve the previous clipboard.
X11 selection ownership MUST remain valid until the replacement owner or window closure ends it.
Wayland paste MUST respect compositor serial and focus requirements.
Unsupported platform formats MUST produce a clear paste error without modifying the draft.

## P08. Provide native open, folder, and save dialogs

The implementation MUST provide Open files, Choose workspace folder, and Save file on every supported desktop.
Dialogs MUST be parented to the initiating window and cancelled when that window generation ends.
They MUST not block the Gio frame owner.

| Platform | Selected API |
|---|---|
| macOS | `NSOpenPanel` and `NSSavePanel`, with folder selection enabled for workspace choice. |
| Windows | `IFileOpenDialog` and `IFileSaveDialog`, including `FOS_PICKFOLDERS`. |
| Linux | XDG FileChooser portal with file, directory, multiple-selection, and save options. |

Cancellation MUST be a distinct result and MUST not create an error notice.
Selection MUST return validated local file references or the portal-provided file access handle.
The adapter MUST not convert an arbitrary remote URL into a local pathname.
It MUST not discard portal access rights before the selected operation finishes.

Save MUST preserve extension filters, suggested names, overwrite confirmation, and atomic-write behavior.
The client MUST validate the selected content after the dialog returns.
An extension filter is a user aid and MUST not replace format validation.

The Linux package MUST declare its portal runtime requirement.
When no portal backend is available, the client MUST show installation guidance and preserve the pending operation.
Every supported Linux release environment MUST include and pass a working portal backend.

## P09. Route menus and shortcuts through one command model

Every menu item, command-palette item, context action, and keyboard shortcut MUST reference one command identifier.
The command model MUST provide its label, enabled state, checked state, target scope, and action.
Platform adapters MUST not duplicate application behavior inside native callbacks.

macOS MUST provide native Application, File, Edit, View, Window, and Help menus.
Windows and Linux MUST provide an accessible application menu and context menus using the shared visual system.
System title-bar menus MUST remain native.

The menus MUST expose thread creation, folder choice, document export, Find, clipboard actions, zoom, settings, diagnostics, and window closure.
Their items MUST reflect the focused field, selected document, active terminal, and current connection state.
Disabled actions MUST retain their label and reason when the reason is actionable.

macOS MUST use Command for standard editing and application shortcuts.
Windows and Linux MUST use Control for the corresponding standard actions.
Displayed shortcut hints MUST match the platform binding.
Menu mnemonics and accessibility activation MUST execute the same command exactly once.

The active modal owns Escape before the underlying view.
The active IME owns composition keys before application commands.
The focused terminal owns terminal keys except explicitly reserved application commands.
Closing a menu MUST restore its previous focus without sending a terminal key or composer action.

Context menus MUST provide keyboard invocation.
They MUST appear within the current display's work area.
They MUST dismiss on focus loss without activating the item under the pointer.

## P10. Integrate external actions and notifications

External authentication MUST open the system browser only after a user invokes sign-in.
This allowed browser launch MUST not create an embedded browser surface.
The client MUST retain the existing provider, callback, cancellation, and credential-storage contracts.
Credentials MUST not enter native logs or clipboard diagnostics.

The platform opener MUST accept only the schemes and operations declared by its caller.
It MUST use structured arguments and native APIs.
It MUST not concatenate a path or URL into a shell command.
Open file and Reveal in folder MUST preserve spaces and non-ASCII paths.

Notifications MUST follow the current [notification behavior](web.md#notifications), adapted to window focus instead of tab visibility.
They MUST announce pending decisions and turn completion states without duplicating visible in-app notices.
The user MUST explicitly enable them.
Notification activation MUST focus the correct window and thread without answering an approval.

macOS MUST use UserNotifications.
Windows MUST use its application-identified desktop notification API.
A Windows click MUST reach dalgona through its registered COM activator, also after dalgona exits.
The activator MUST report each click once, with the thread tag from the notification's `launch` argument.
Linux MUST use the notification portal or its declared freedesktop desktop service.
The adapter MUST expose granted, denied, unavailable, and undecided states accurately.

An unavailable notification service MUST not prevent conversation use.
The Settings component MUST explain the state and preserve the user's preference.
Closing or clicking a notification MUST not alter the daemon's execution state.

## P11. Implement desktop accessibility bridges

The client MUST expose a platform-independent semantic tree backed by its document and component models.
Its node identities MUST remain stable across frames and virtualization.
Roles, names, descriptions, states, bounds, relationships, and actions MUST reflect actual application state.

Gio's semantic operations are an input to this model.
They MUST not be treated as proof of complete desktop screen-reader support.
The implementation MUST provide the following native bridges:

| Platform | Required bridge and behavior |
|---|---|
| macOS | `NSAccessibilityElement` and role-specific AppKit accessibility protocols, including text ranges, selection, bounds, and notifications. |
| Windows | UI Automation providers, including fragment navigation, actions, values, selection, scrolling, text ranges, and virtualized document access. |
| Linux | AT-SPI2 D-Bus objects, including Accessible, Component, Action, Text, EditableText, Selection, Table, and relevant state events. |

The selected approach is direct native adapters around one Go semantic model.

| Candidate | Consequence | Decision |
|---|---|---|
| AccessKit through its C bindings | Adds Rust and C artifacts while still requiring application semantics and text integration. | Rejected for this Go client. |
| Direct platform adapters | Keeps semantic ownership in Go and makes each platform contract explicit. | Selected, with all three bridges required. |

The Windows adapter MUST register its provider through the native window's UI Automation route.
It MUST handle `WM_GETOBJECT` and return providers through `UiaReturnRawElementProvider` where required.
The adapter MUST preserve Gio's existing window message handling.

The macOS adapter MUST attach semantic elements to the current Gio view.
It MUST implement text range, string, line, selection, and coordinate queries for document and editor roles.
The Linux adapter MUST register and unregister its objects with the accessibility bus across connection changes.
It MUST register with each registry once, and again only when another registry announces that it is available.

Native coordinate and character-offset conventions MUST be converted at the adapter boundary.
Queries MUST use immutable snapshots with a known revision.
Actions MUST revalidate their target revision before changing state.
Stale node identifiers MUST return an unavailable-element result rather than act on a reused identifier.

The semantic model in `internal/dalgona/gui/platform` maps each role to one Gio semantic class.

| Semantic role | Gio class |
|---|---|
| Button, link, and menu item | `Button` |
| Check box | `CheckBox` |
| Radio button | `RadioButton` |
| Switch | `Switch` |
| Text field and secure field | `Editor` |
| Document, heading, list, list item, table, row, column header, row header, cell, code, math, figure, image, terminal, dialog, menu, status, and error | `Unknown` |

Only check boxes, radio buttons, and switches send a checked state to Gio.
A disabled control adds its reason to the Gio description.
A secure field sends its name to Gio and never its value.

## P12. Make accessibility behavior complete

Every control MUST expose its name, role, enabled state, and operation.
Every error MUST associate with the affected field or action.
Pending approvals MUST expose command, workspace, reason, and each decision independently.
Keyboard focus and accessibility focus MUST remain synchronized through dialogs, overlays, thread changes, and disappearing controls.

The transcript MUST expose headings, list structure, table relationships, code language, links, and complete readable text.
It MUST support text ranges and selection across virtualized blocks.
Native accessibility callbacks MUST answer from bounded immutable semantic snapshots and cached text ranges.
They MUST not perform filesystem access, network access, source paging, or renderer work synchronously.
One callback MAY return at most 64 KiB of text through a bounded semantic text object.
The semantic tree MUST expose further text objects and their logical order without truncating the document's navigation model.

A query requiring uncached content MUST return the platform's supported busy state or unavailable response.
It MUST enqueue an asynchronous document fetch with the requested identity, revision, and range.
Duplicate requests for that range MUST share the pending fetch.
The adapter MUST preserve focus and the logical navigation target while the fetch runs.

Completion MUST publish a new semantic snapshot and send the platform's appropriate content or state notification.
The pending navigation action MUST then resume if its identity and revision remain valid.
Failure MUST expose a readable error and retry action without fabricating empty text or invalid bounds.
It MUST not report only the currently painted lines as the complete document.

The composer MUST expose editable text, selection, caret, placeholder, validation errors, and composition-safe replacement.
Secure fields MUST expose their protected state without exposing their raw value.
Copy and select actions MUST obey the same document boundaries as keyboard actions.
Platform text-range units MUST convert explicitly to canonical source offsets before editing or copying.
Font fallback, visible control escapes, variation sequences, and joined emoji MUST retain source-to-display mappings for those ranges.
Painted fallback text MUST NOT become the copied or edited value.
Accessibility edits MUST pass the same composition ownership, grapheme, encoding, and atomic limit checks as keyboard and clipboard edits.

Math MUST expose the semantic expression defined in [native documents](native-documents.md#d09-render-mathematical-notation).
Figures MUST expose title, caption, source access, and semantic labels in meaningful reading order.
Images MUST expose their attachment names, alternative text when provided, and preview controls.
The terminal MUST expose its current readable screen and retained scrollback with explicit region boundaries.

Streaming MUST not announce every token.
Status announcements MUST coalesce ordinary progress to at most one update per second.
Pending decisions, completion, and failures MUST remain timely and distinct.
A user reading earlier text MUST not lose accessibility focus when new output arrives.

The client MUST honor system high-contrast preferences and reduced motion.
Every operation MUST remain reachable through the keyboard.
Focus order MUST match the document and component order.
No action MAY rely only on color, hover, dragging, or an unlabeled icon.

## P13. Bind real terminals to their operating system

The daemon owns terminal processes under the [native protocol](native-protocol.md).
The GUI MUST not start a local shell to imitate a missing remote terminal.
The workspace binding and terminal identity MUST remain visible in the Terminal component.

Unix implementations MUST use a PTY with a controlling terminal and appropriate process-group ownership.
Windows MUST use ConPTY with independent input and output servicing.
The [ConPTY contract](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session) requires careful draining during creation, resizing, and closure.

The runner owner MUST implement open, input, output, resize, signal, exit, and cleanup behavior on each supported platform.
Its implementation MUST preserve process ownership and terminal lease rules.
Closing a terminal MUST drain final output before reporting its final state.
An unknown process outcome MUST not be represented as a successful exit.

The GUI MUST use the terminal methods and chunked checkpoint transfer defined by [R12](native-protocol.md#r12-real-terminal-sessions).
It MUST validate the complete checkpoint digest, schema, and engine version before applying that checkpoint atomically.
It MUST preserve input sequence numbers across retried writes.
It MUST apply the latest successful resize sequence and avoid oscillating sizes during layout.
An output gap MUST trigger a fresh atomic attachment snapshot.

The snapshot adapter MUST preserve both screens, scrollback, cursor, modes, margins, tabs, attributes, links, and parser continuation.
The public VT emulator API MUST not be assumed to provide that complete serialization.
Required checkpoint support MUST live outside the GUI package so the daemon does not import presentation code.
A mismatch of the snapshot version or the engine version MUST fail explicitly and request a compatible attachment.
If the complete scrollback does not fit in the 8 MiB checkpoint body, the checkpoint MUST drop the oldest scrollback rows. It MUST report the number dropped in `droppedScrollbackRows`.
The GUI MUST show that number at the retained-history boundary.

Closing a window with active terminals MUST explain which terminal sessions will close and obtain the component's close decision.
Closing the Terminal panel only detaches its view, as the [component contract](native-components.md) specifies.
Stop terminal is the explicit action that closes its server process.
GUI exit MUST not send an implicit agent interrupt.
The daemon MUST retain its existing subscriber-loss policy for unanswered approvals and questions.
Temporary disconnection MUST follow the protocol's thirty-second terminal lease.
Daemon restart MUST invalidate terminal identifiers rather than silently start replacement shells.

## P14. Build complete desktop packages

Each published package MUST include the GUI, compatible daemon executable, required fonts, marks, licenses, and renderer assets.
The renderer child MUST come from the same signed executable version as its parent.
The client MUST not download executable components after installation.

| Platform | Required artifacts and integration |
|---|---|
| macOS | A signed and notarized universal `.app` distributed in a signed `.dmg`, with bundle identity, icon, and native menus. |
| Windows | Separate signed `.msi` packages and executables for x64 and Arm64, with application identity, Start menu entry, and uninstall support. |
| Linux | Ubuntu 24.04 LTS `.deb` packages for amd64 and arm64, with desktop entry, icon, MIME-independent launch, and declared portal dependencies. |

The following table fixes the initial operating-system acceptance baseline.
Every architecture MUST run its native binary on the declared platform.
An emulated x64 binary MUST not satisfy an arm64 gate.

| Target | Operating-system baseline | Required desktop environment | Package |
|---|---|---|---|
| `linux/amd64` | Ubuntu 24.04 LTS | GNOME with separate native X11 and Wayland runs | amd64 `.deb` |
| `linux/arm64` | Ubuntu 24.04 LTS | Raspberry Pi 5 desktop image, with separate native X11 and Wayland runs | arm64 `.deb` |
| `darwin/amd64` | macOS Tahoe 26 | An Intel Mac on Apple's Tahoe compatibility list | Universal `.app` and `.dmg` |
| `darwin/arm64` | macOS Tahoe 26 | An Apple silicon Mac on Apple's Tahoe compatibility list | Universal `.app` and `.dmg` |
| `windows/amd64` | Windows 11, version 25H2 | Windows desktop on supported x64 hardware | x64 `.msi` |
| `windows/arm64` | Windows 11, version 25H2 | Windows desktop on supported Arm64 hardware | Arm64 `.msi` |

Windows acceptance MUST cover Home and Pro editions for both architecture targets.
These release families are fixed baselines, not aliases for the newest operating system.
Every [verification record](native-verification.md) MUST include the exact edition, patch version, OS build, installed updates, and installation-image digest.
Linux records MUST also identify the kernel, desktop packages, compositor, graphics driver, and portal backend.
The same recorded system image MUST be reusable for regression testing.

Ubuntu's [24.04 release notes](https://documentation.ubuntu.com/release-notes/24.04/) document Raspberry Pi 5 arm64 desktop support and the Xorg and Wayland environments.
The Linux support claim is limited to the Ubuntu baseline in this table.
Another distribution or desktop requires its own declared acceptance matrix.

Apple's [Tahoe compatibility list](https://support.apple.com/en-us/122867) includes the 16-inch 2019 MacBook Pro.
Its [processor specification](https://support.apple.com/en-us/111932) confirms Intel processors, so the macOS 26 baseline includes a native amd64 test route.
The same compatibility list includes Apple silicon models for the arm64 route.

Microsoft's [25H2 update catalog](https://www.catalog.update.microsoft.com/Search.aspx?q=Feature%20Update%2025H2) lists separate x64 and Arm64 updates.
Its [lifecycle table](https://learn.microsoft.com/en-us/lifecycle/products/windows-11-home-and-pro) includes version 25H2 for Home and Pro.
The Arm64 MSI MUST use the [required installer schema and platform metadata](https://learn.microsoft.com/en-us/windows/win32/msi/using-64-bit-windows-installer-packages).
The package MUST use schema 500 or later and the `Arm64` platform value.

Gio's [Linux installation guide](https://gioui.org/doc/install/linux) lists native graphics and window-system build dependencies.
The package build MUST provide them in declared build environments.
Gio's [macOS guide](https://gioui.org/doc/install/macos) requires Xcode.
Its [Windows guide](https://gioui.org/doc/install/windows) does not remove dependencies added by this client's native bridges.

Builds MUST use pinned Go, Gio, engine, and asset revisions.
The six target pairs appear in Go's [supported target table](https://go.dev/doc/install/source#environment).
That compiler support MUST not substitute for the native Gio and platform acceptance gates.
The GUI dependency boundary MUST preserve headless CLI and daemon builds.
The implementation MUST update dependency rules and build targets without requiring a display for ordinary headless commands.
Native smoke tests MUST run on actual display sessions in addition to compile checks.

### Required build targets

The implementation MUST register the `gui` build tag for the desktop command and files that import Gio or native GUI adapters.
Pure state, protocol, parser, and document-position code MUST remain testable without that tag or a display.
Display-dependent tests MUST additionally require the `guiintegration` tag.
Excluding those tests from a headless run MUST NOT count as passing native acceptance.

| Target | Required behavior |
|---|---|
| `desktop-build` | Build `cmd/dalgona-gui` with `-tags gui`, the pinned toolchain, native compiler requirements, and packaged asset inputs. |
| `desktop-check` | Run formatting, vet, lint, race tests, vulnerability checks, and builds with `gui` included in their effective build configuration. |
| `desktop-smoke` | Run `gui,guiintegration` tests on the selected native display backend and produce the platform evidence record. |
| `desktop-package` | Assemble and validate the platform artifact with its compatible daemon, assets, notices, signatures, and installation tests. |

Existing `build`, `dist`, `box-dist`, and `verify` targets MUST retain their headless dependency path.
Desktop CI MUST run the tagged checks for every target in the initial release matrix.
Linux smoke evidence MUST include separate X11 and Wayland runs.
The CI record MUST distinguish a native execution result from a cross-compilation result.

The `justfile` declares the four targets.
On Linux they drop a window backend or the Vulkan renderer only when its development files are absent, and they print that choice.
With `CI=true`, a missing backend or renderer fails the target.
`desktop-smoke` takes the backend name `wayland` or `x11` and fails without a matching display session.
`desktop-package` dispatches on the host OS.
macOS runs `deploy/desktop/package-macos.sh`.
Windows runs `deploy/desktop/package-windows.ps1`.
Linux builds the `.deb` for the host architecture only and requires both window backends.
The Linux package builds only for amd64 or arm64 and needs both window backends. It takes its version from the exact `v` tag with the `v` removed or from `0.0.0~gitYYYYMMDD.<12-character commit>`.
The macOS recipe builds a universal app and DMG, signs both with `DALGONA_MACOS_SIGN_IDENTITY`, notarizes and staples with `DALGONA_MACOS_NOTARY_PROFILE`, and refuses when either input is absent.
The Windows recipe builds natively on matching x64 or Arm64 hardware.
It signs binaries and the per-machine MSI with `DALGONA_WINDOWS_SIGN_SHA1` plus `DALGONA_WINDOWS_TIMESTAMP_URL`.
It refuses when either input is absent.
The Linux `.deb` is unsigned and its install, upgrade, launch, and uninstall tests need a clean Ubuntu 24.04 system and did not run.
CI `desktop-macos` and `desktop-windows` jobs validate manifests without signing or publishing and run `desktop-check` plus `desktop-build`.
Installation MUST not overwrite another running daemon's data or change its credentials.
Uninstall MUST remove application files while preserving user data unless the user explicitly chooses data removal.
An upgrade MUST preserve drafts, preferences, daemon state, and valid file associations.

### Windows notification registration

Windows shows dalgona notifications only for an installed application identity.
The MSI MUST install one Start menu shortcut that starts `dalgona-gui.exe`.
The shortcut MUST have the two properties in this table.

| Shortcut property | Value |
|---|---|
| `System.AppUserModel.ID` | `Gosuda.Dalgona` |
| `System.AppUserModel.ToastActivatorCLSID` | `{4C824C81-04CD-4609-A755-D11526E6CDFE}` |

The MSI MUST also register the activator class.
Write the quoted full path of the installed `dalgona-gui.exe` as the default value of this key:
`Software\Classes\CLSID\{4C824C81-04CD-4609-A755-D11526E6CDFE}\LocalServer32`.
Use `HKEY_LOCAL_MACHINE` for a per-machine installation.
Use `HKEY_CURRENT_USER` for a per-user installation.
When dalgona does not run, a click starts this path with the `-Embedding` argument.
Then dalgona registers the activator and gets the thread tag from Windows.
Without the shortcut, Windows does not show the notifications.
Without the key, a click after dalgona exits does not start dalgona.
The uninstaller MUST remove the shortcut and the key.

## P15. Verify platform completion

The platform owner MUST deliver the following release evidence.
Each row is required on its applicable operating systems and architectures.

| Gate | Required proof |
|---|---|
| Handle lifecycle | Repeated window creation, closure, display changes, and adapter cancellation leave no stale native handle or callback. |
| Rendering | Idle waits, active streaming, reduced motion, device recovery, and mixed-scale displays preserve usable frames. |
| IME | P05 composition cases pass with every declared Korean, Japanese, and Chinese engine and real candidate windows. |
| Clipboard | Text, platform image formats, file references, cancellation, and delayed paste target checks pass. |
| Dialogs | Open, multiple selection, folder choice, save, cancellation, overwrite refusal, and portal paths pass. |
| Menus | Keyboard, pointer, native menu, and command-palette activation execute one identical command. |
| Accessibility | VoiceOver, Windows Narrator, NVDA, and Orca complete every applicable core task without a pointer. |
| Text accessibility | Each screen reader reads, selects, copies, edits, and navigates text beyond the mounted viewport. |
| Terminal | Real Unix PTY and ConPTY sessions pass resize, input, disconnection, close, and process-cleanup cases. |
| Packaging | Install, offline launch, upgrade, launch from system menus, and uninstall pass on clean supported systems. |

Core screen-reader tasks MUST include sign-in, thread creation, message entry, approval, question answering, review, file selection, terminal use, and error recovery.
The evidence MUST include real assistive software, not only semantic-tree snapshots.
Passing keyboard navigation alone MUST not count as passing screen-reader support.

The implementation MUST record each missing bridge behavior against its owning package and this specification's identifier.
Any missing required behavior blocks a complete desktop release.
An implementation stage MAY be reviewed before that release, but MUST retain its incomplete status accurately.

### Platform evidence record

The `EvidenceRecord` type in `internal/dalgona/gui/platform` defines the JSON record of one smoke run.
The encoder and the decoder refuse a record that breaks one of these rules.

| Member | Rule |
|---|---|
| `commit` | The full lowercase commit hash with 40 or 64 digits. |
| `pins` | Each module once with its exact version. The `go` and `gioui.org` pins are required. |
| `host.os` and `host.arch` | One target from the P14 baseline table. |
| `host.display.backend` | `x11` or `wayland` on Linux, `appkit` on macOS, and `win32` on Windows. |
| `host.display.virtual` | The name and version of a virtual display. It stays empty for a physical display session. |
| `host.execution` | `native`, `emulated`, or `cross-compiled`. Only a `native` run can pass a check. |
| `host.edition`, `host.build`, and `host.updates` | The edition, the OS build, and each installed update. |
| `host.image_digest` | The installation image as `sha256:` and 64 lowercase hex digits. |
| `host.gpu`, `host.driver`, `host.locale`, and `host.input_method` | Each value is required. |
| `host.linux` | The kernel, compositor, desktop packages, and portal backend. It is required on Linux and absent on other systems. |
| `checks` | The gate, kind, exact procedure, expected result, actual result, and outcome. A pass or a failure also names its artifact. |
| `missing` | A P01 to P15 identifier, the owning package path, and the missing behavior. |

The check kinds are `automated`, `inspection`, `manual-native`, and `live-service`.
The outcomes are `passed`, `failed`, and `skipped`.
An IME, accessibility, or text accessibility pass MUST be a `manual-native` check.
Its `software` member MUST name the real input engine or assistive software and its version.

The `gui,guiintegration` tests in `internal/dalgona/gui/platform` write this record when `DALGONA_EVIDENCE_RECORD` names an output file.
They read the host facts from the record that `DALGONA_EVIDENCE_HOST` names and add one automated check for each smoke test.
Each check names the log file from `DALGONA_EVIDENCE_LOG` as its artifact.
`just desktop-smoke <backend>` writes the host description and sets these three variables.
It reads the OS edition, OS build, kernel, desktop packages, portal backend, and locale from the host.
It takes the installation image digest from `DALGONA_IMAGE_DIGEST` and the installed updates from `DALGONA_HOST_UPDATES`, one update on each line.
It takes each fact that it cannot detect from an environment variable.
These are `DALGONA_HOST_GPU`, `DALGONA_HOST_DRIVER`, `DALGONA_COMPOSITOR`, `DALGONA_PORTAL_BACKEND`, `DALGONA_INPUT_METHOD`, and `DALGONA_DISPLAY_VIRTUAL`.
`DALGONA_EXECUTION` records an `emulated` or `cross-compiled` run.
When a fact is missing, the target still writes the text log, names each missing fact, and fails.
The record is `dist/desktop-smoke-<os>-<arch>-<backend>.json`.

## Implementation status

This section records the owned work in `internal/dalgona/gui/platform` and `internal/dalgona/gui/platform/desktop`, and each missing behavior.
It records no passed release gate.
Nothing in this table has run on Windows, macOS, or a Linux desktop session with real assistive software or input methods.
It does not report P01, P02, P04, P06, P09, or P13.

| Requirement | Implemented | Missing behavior and owner |
|---|---|---|
| P03 | `Lifetime` starts a new generation for each valid view event. Ending it cancels pending work, then releases each attached adapter once. `desktop.Window` attaches the accessibility bridge and the clipboard reader to that generation. Each window loop in `cmd/dalgona-gui` sends every event to `desktop.Window.HandleEvent`. A new view generation ends the open dialog of the earlier generation with an error. | Wayland exposes no screen origin to the accessibility bridge. |
| P05 | `platform.Editor` wraps `widget.Editor`. It takes the IME events before the widget and reports `Composing()`. Preedit stays out of `Draft()`. Each commit passes the field policy once and is one undo step. The editor keeps the newest 1000 undo steps within 32 MiB. Outside composition, it replaces the Gio widget after 1000 widget edits or 32 MiB, and it keeps the text, selection, and focus. It asks its `Clipboard` function for a reader at each paste. | A widget replacement scrolls to the caret, so a field scrolled away from the caret moves once. The composer and panel fields do not use `platform.Editor` yet. |
| P05 | Every key belongs to the IME during composition. A Return, Enter, or Escape in the same frame as the end of a composition also belongs to it. A rejected commit restores the text and keeps the candidate. | No real IME matrix result exists. Windows may deliver the Return that ends a Korean composition in a later frame. |
| P07 | Owned readers replace the Gio text read. Windows reads `CF_UNICODETEXT` strictly, the registered `PNG` format, `CF_DIBV5` and `CF_DIB` as PNG, and `CF_HDROP`. A pure Go X11 client reads text, PNG, JPEG, and URI lists with incremental transfer. macOS reads text, PNG, TIFF as PNG, and file URLs. On a Wayland view, a pure Go client on its own connection reads text, PNG, JPEG, and URI lists through `ext-data-control-v1`, else `wlr-data-control`. It binds the first announced seat and negotiates the offered MIME spelling. Data control needs no input serial, and the reader runs only for a paste in the focused window. | Mutter offers neither data-control protocol, so GNOME without XWayland falls back to the unbounded Gio text read. The data-control reader has not run against a real compositor. Gio v0.10.3 has no external drops. The macOS reader has not compiled against the Apple SDK. |
| P07 | Each reader bounds a transfer at 32 MiB before decoding. `ChoosePaste` prefers files, then text, then an image for Paste, and an image first for Paste image. `PasteTarget` and the editor discard a paste when the target changed. | The X11 path through XWayland on GNOME has not run. |
| P08 | Linux uses the XDG FileChooser portal directly. It supports several files, folders, save names, filters, cancellation, and the X11 parent window. It closes the request when the window generation ends. Windows uses its own COM thread, and the Gio window owns its dialogs. `IFileOpenDialog` uses `FOS_ALLOWMULTISELECT` or `FOS_PICKFOLDERS`. `IFileSaveDialog` uses `FOS_OVERWRITEPROMPT`, `FOS_STRICTFILETYPES`, and `FOS_NOTESTFILECREATE`. macOS runs `NSOpenPanel` and `NSSavePanel` as sheets on the Gio window. Both return paths without creating the file. Both accept only file-system paths and close the dialog when the window generation ends. | The Windows and macOS dialogs have not run on their release targets. The macOS bridge has not compiled against the Apple SDK. Wayland has no parent handle. |
| P11 | `AttachATSPI` exports a frame and the semantic tree on the accessibility bus. It exports Accessible, Application, Component, Action, Text, EditableText, Selection, Table, TableCell, and Cache. Its Text interface reads character, word, sentence, line, and paragraph segments from the shared semantic text model. `GetCharacterExtents`, `GetRangeExtents`, and `GetOffsetAtPoint` use line geometry. `AttachUIA` registers UI Automation providers on the Gio window. Its word unit follows Unicode word boundaries. `GetBoundingRectangles` and `RangeFromPoint` use line geometry. `AttachAppKitAccessibility` adds elements to the Gio view. It answers line, frame, and position queries from the same lines. A `SemanticNode` carries `Lines`. Each line has a byte range, a line box, and one horizontal span for each grapheme cluster. Visible transcript rows supply these lines. If text has no lines, a range reports the node box. Characters and points report no geometry in this case. Table cells carry row and column spans. Row headers expose their cells on all three bridges. The frame dispatches accepted accessibility intents on the next frame after publication, not in the frame that queued them. | UI Automation has no sentence unit. Its line unit still breaks only at hard line breaks. The AppKit bridge has not compiled against the Apple SDK. |
| P12 | The window publishes semantic snapshots and Gio key focus to the native bridges. The composer exposes canonical text and selection. Protected fields do not expose their values. Decision cards expose each action and its details. The transcript exposes nested lists, tables with column headers, figure captions and labels, and math expressions. Figure source and image preview controls report their visible bounds. Native ranges span ordered text objects. Native selection uses the same document positions and copy rules as keyboard selection. Native queries report missing text as unavailable and share one read per missing object. The client publishes the result before it resumes navigation for the same revision. Failed reads expose an error and a retry action. The terminal exposes separate screen and scrollback regions. The daemon sends attachment names and caller-provided alternative text on image entries as name and altText, with Path-basename then Image-N fallback when empty. | Full-source pages are outside the keyboard selection scope and support reading only. The AppKit bridge has not compiled against the Apple SDK. All three release targets still need live screen-reader checks. |
| P12 | `ReducedMotion`, `HighContrast`, and `PrefersDark` read the settings portal on Linux, `SystemParametersInfoW` and the `AppsUseLightTheme` value on Windows, and `NSWorkspace` and the global `AppleInterfaceStyle` on macOS. `WatchAppearance` reports changes through the portal `SettingChanged` signal, `WM_SETTINGCHANGE` on a hidden window, and appearance observation on macOS. The application rereads all three after each change and when a window gains focus. | The Windows and macOS watchers have not compiled against their SDKs. |
| P15 | `EvidenceRecord` defines and validates the record format. `desktop-smoke` writes it. The `guiintegration` tests check the handle lifecycle and an exact clipboard round trip on a real display. | No other gate has a check. The smoke tests have not run on a release target. |
| P10 | `Notifier` uses the freedesktop notification service on Linux, WinRT toasts with the `Gosuda.Dalgona` application identity on Windows, and UserNotifications on macOS. A notice with the same tag replaces the older one, and each click reports its tag. On Windows, each toast carries its tag as the `launch` argument. The notification thread registers the COM activator `{4C824C81-04CD-4609-A755-D11526E6CDFE}`, and a click that reaches both the activator and the toast event reports once. `OpenLink` opens only `https`, `http`, and `mailto`. `OpenURL`, `OpenPath`, and `RevealPath` use the OpenURI portal, `ShellExecuteW`, and `NSWorkspace`. | No Windows click has run, with or without a running dalgona. The macOS notifier has not compiled against the Apple SDK. |
| P14 | `just desktop-package` dispatches per host OS. On macOS, `deploy/desktop/package-macos.sh` and `Info.plist` assemble a signed and notarized universal app and DMG. On Windows, `deploy/desktop/package-windows.ps1` and `dalgona.wxs` assemble signed native x64 and Arm64 per-machine MSIs. They include the Start menu AUMID and quoted `HKLM` toast activator registration. The Linux `.deb` recipe is unchanged. | Native DMG/MSI assembly, signing, and notarization did not run. The T24 install, upgrade, launch, uninstall, and retention matrix did not run. Neither native package assembly nor that matrix is proven. Only Linux static checks ran: `bash -n`, `xmllint`, `actionlint`, `just --show desktop-package`, and missing-input refusal. No artifact was signed or published. Owner: NativePackaging recipes. This table records status only. |
Each missing behavior in this table belongs to `internal/dalgona/gui/platform` unless the row names another owner.

## Primary sources

- [Gio project](https://gioui.org/).
- [The requested Gio Context7 reference](https://context7.com/git_git_sr_ht/~eliasnaur_gio).
- [Current Gio application API](https://pkg.go.dev/gioui.org/app).
- [Current Gio text widgets](https://pkg.go.dev/gioui.org/widget).
- [Gio text clipboard API](https://pkg.go.dev/gioui.org/io/clipboard).
- [Gio Linux IME changes](https://gioui.org/news/2026-09).
- [Gio semantic operations](https://github.com/gioui/gio/blob/main/io/semantic/semantic.go).
- [AppKit accessibility protocol](https://developer.apple.com/documentation/appkit/nsaccessibilityprotocol).
- [Windows UI Automation provider attachment](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-howto-expose-serverside-uiautomation-provider).
- [AT-SPI text interface](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/iface.Text.html).
- [AT-SPI editable text interface](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/iface.EditableText.html).
- [AccessKit's C bindings and build requirements](https://github.com/AccessKit/accesskit-c).
- [XDG FileChooser portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html).
- [Windows pseudoconsole lifecycle](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session).
- [Windows pseudoconsole stream encoding](https://learn.microsoft.com/en-us/windows/console/createpseudoconsole).

The platform owner MUST verify indexed examples against the pinned upstream source.
Documentation availability alone MUST not establish implemented support.
