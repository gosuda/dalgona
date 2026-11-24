# Native design

Status: Required design. `internal/dalgona/gui/theme` implements the V02 palettes, the V03 hue rule, and the V05 bindings. The shell implements part of V06 and V07. This document certifies no desktop client.
The source baseline is `2c036c60b379e7718255a23ebd2c5079d76ec50c`.

## V01: Authority and scope

The Gio client MUST follow the visual system in [DESIGN.md](../../../DESIGN.md).
The [native decision](../decisions/native-client.md) selects Gio and the native client boundary.
The [native overview](native.md) defines completion.
The [component contract](native-components.md) defines interactions.

This document registers native bindings for existing design values.
It also registers the native additions stated below.
These visual bindings MUST NOT change existing terminal or browser design.
This specification PR MUST NOT change values in `DESIGN.md`.

The client MUST retain the browser's thread navigation, workspace, and composer structure.
Each additional feature MUST use a compact panel or an existing control.
Every visible element MUST support a named user action or state.
The client MUST NOT add a permanent dashboard for auxiliary features.

The implementation MUST use the requirement identifiers in this document for [verification](native-verification.md).

## V02: Themes and color roles

The theme setting MUST accept these values.

| Setting | Native behavior |
|---|---|
| `auto` | Follow the operating system and choose `night` or `day`. |
| `auto-soft` | Follow the operating system and choose `soft-night` or `soft-day`. |
| `night` | Use the fixed night palette. |
| `soft-night` | Use the fixed soft night palette. |
| `day` | Use the fixed day palette. |
| `soft-day` | Use the fixed soft day palette. |

An unknown stored theme MUST display as `auto` and MUST NOT be written back without a user action.
The client MUST apply its cached theme before the first visible frame.
Daemon settings MUST replace that cache after initialization.
System theme changes MUST update a following theme while the window remains open.
The cached theme MUST contain no credential or workspace content.

The palette MUST use the exact values in [Colour](../../../DESIGN.md#colour).
That table remains the single authority for every theme role.
The client MUST NOT introduce an additional palette or gradient.

Only `canvas`, `surface`, `tone`, and the point MUST provide control fills.
The product mark retains its separate registered brand colors.
Failure, addition, and warning roles MUST color text rather than control backgrounds.
The `faint` role MUST remain decorative.
Text MUST NOT use `faint`.
Syntax and diagram colors MUST resolve through these roles under the [document contract](native-documents.md).

## V03: The point and thread hue

Each view MUST show at most one solid accent control fill.
The point holder MUST follow this order.

| Priority | Holder |
|---|---|
| First | The primary action of the active request or modal panel. |
| Second | The live state tag of the running open thread. |
| Third | Send in an idle open thread. |
| Fourth | Start thread in the empty workspace. |

The sidebar Start thread control MUST remain quiet.
An inactive window MUST retain the same point assignment.
Focus MUST use its own ring rather than taking the point fill.
Thread-list hue dots MUST retain their existing identity role.
They MUST NOT become additional filled action controls.

Each thread hue MUST equal `fnv1a32(threadID) % 4` over UTF-8 bytes.
The Go implementation MUST use `hash/fnv.New32a`.
A view without an open thread MUST carry no thread hue.
Its point fill uses hue 1, the empty-view fill of the browser.

The hue order, fills, and inks MUST match [the thread-hue table](../../../DESIGN.md#the-point-and-the-thread-hue).
Those values MUST NOT be copied into another normative palette table.

Soft themes MUST use the corresponding night or day hue pair.
The GUI MUST preserve this registered palette rather than introducing an unrelated desktop accent.

## V04: Typography and text

| Role | Native face |
|---|---|
| Titles and control labels | Bricolage Grotesque, then Pretendard. |
| Prose | Hanken Grotesk, then Pretendard. |
| Hangul | Pretendard through the registered fallback chain. |
| Code and terminal | The user's configured system monospace face. |
| Registered icons | Symbols Nerd Font Mono 3.5.1 subset. |

The [platform contract](native-platform.md) defines font packaging and fallback discovery.
The release MUST include required font licenses.
The client MUST use native font data supported by its text shaper.
It MUST NOT require CSS or a browser font loader.

Prose MUST preserve whole Korean words where space permits.
An overlong token MUST wrap within the available width.
Code MUST preserve source whitespace and expose horizontal scrolling inside its explicit code viewer when needed.
Window resizing MUST NOT change stored text, selection anchors, or copied bytes.
Elapsed values, counts, and percentages MUST use tabular figures.
Missing glyphs MUST trigger the registered fallback chain.
The client MUST NOT replace valid input with a guessed character.

## V05: Native token bindings

All geometry MUST use named tokens.
Geometry values below use Gio logical pixels, represented by `unit.Dp`.
Text sizes use `unit.Sp` and MUST respect the user's text scale.
Physical pixels MUST be derived from the current window metric.

Every entry in [Scale](../../../DESIGN.md#scale) MUST receive its corresponding native binding.
The [component token bindings](../../../DESIGN.md#component-token-bindings) remain authoritative for component-specific geometry.
Native identifiers MUST retain each source token's identity through a documented mechanical name mapping.
For example, `--space-8` maps to `space8`.
The Go package `internal/dalgona/gui/theme` exports each identifier with an upper-case first letter, so `space8` is `theme.Space8`.

Existing pixel geometry MUST map one-to-one to dp at the default desktop metric.
Existing text sizes MUST map one-to-one to sp before user scaling.
Unitless line heights MUST retain their ratios.
Font weights MUST retain their registered numeric values.
Character-based widths MUST use measured advances from the selected font.
The implementation MUST NOT reinterpret component-showcase geometry as application-shell geometry.

The native client MUST register these additional bindings.

| Native-only token | Value | Origin |
|---|---|---|
| `inputTextSize` | 16 sp | Native mapping of the browser accessibility minimum. |
| `windowDefaultWidth` | 1280 dp | Desktop first-open client-area width. |
| `windowDefaultHeight` | 800 dp | Desktop first-open client-area height. |
| `windowMinWidth` | 320 dp | Small-window width contract. |
| `windowMinHeight` | 320 dp | Desktop minimum with scrollable panels. |
| `sidebarMinWidth` | 256 dp | Existing `16rem` shell minimum. |
| `sidebarMaxWidth` | 304 dp | Existing `19rem` shell maximum. |
| `transcriptMaxWidth` | 896 dp | Existing `56rem` content maximum. |
| `shellStackWidth` | 832 dp | Existing `52rem` shell breakpoint. |
| `threadListStackCap` | 288 dp | Existing `18rem` list cap. |
| `statusCompactWidth` | 640 dp | Existing `40rem` header breakpoint. |
| `statusDropFolderWidth` | 480 dp | Existing `30rem` band threshold. |
| `statusDropQueueWidth` | 416 dp | Existing `26rem` band threshold. |
| `statusDropEffortWidth` | 256 dp | Existing `16rem` band threshold. |
| `statusDropMarkWidth` | 192 dp | Existing `12rem` band threshold. |
| `followDistance` | 32 dp | Existing transcript follow threshold. |
| `requestScrollStep` | 48 dp | Existing request scrolling increment. |
| `modalMaxWidth` | 1120 dp | Existing `70rem` modal maximum. |
| `idleDockClearance` | 96 dp | Existing `6rem` narrow-window clearance. |
| `titleBasis` | 256 dp | Existing `16rem` header title basis. |
| `bandMinWidth` | 128 dp | Existing `8rem` status band minimum. |

The rem conversions above use the browser's 16-pixel baseline.
Text scaling MUST NOT change those logical geometry definitions.
Minimum heights MUST expand when scaled text or wrapped content needs more space.

The native `inputTextSize` overrides the generic 15-pixel Field input value only for editable native form text.
This override implements the stronger 16-pixel form requirement in [the web contract](web.md#accessibility).
It MUST NOT change the 15-sp prose size.
No existing browser or terminal token changes in this PR.

## V06: Shell and responsive layout

The main shell MUST follow [App.svelte](../../../web/src/lib/components/App.svelte).
Above `shellStackWidth`, it MUST contain a sidebar and a workspace.
The sidebar MUST merge threads from every configured endpoint into endpoint lanes.
Each lane header MUST show the endpoint display label and its phase word.
The phase words display sentence case for the canonical phase tokens `idle`, `connecting`, `connected`, `reconnecting`, `offline`, `incompatible`, and `failed`.
`Idle` MUST NOT carry a warning or failure treatment.
A lane header MUST use text roles only and MUST NOT add a lane fill.
It MUST NOT use the point, thread hue, or a per-endpoint accent.
The sidebar MUST stay between its registered minimum and maximum widths when space permits.
The workspace MUST contain the header, transcript, and dock in that order.
The transcript MUST scroll within its workspace column.

At or below `shellStackWidth`, the shell MUST stack navigation above the workspace.
The thread list MUST have its registered height cap.
The transcript MUST share the workspace scroll surface in this layout.
The dock MUST remain reachable at the window bottom.
Its measured height MUST reserve space after the final transcript row.
An idle dock MUST preserve `idleDockClearance` when the available window height permits it.

At minimum dimensions, every action MUST remain reachable through explicit scrolling.
The shell MUST NOT acquire horizontal scrolling from prose or controls.
Long model names, paths, errors, and titles MUST wrap or truncate through their declared rules.
Complete values MUST remain available to selection, details, or accessible descriptions.
The narrow layout MUST NOT remove a required action.

Files, terminals, remote management, diagnostics, and reports MUST use the common panel shell.
Opening them MUST NOT permanently add workspace columns.
Closing a panel MUST restore the prior transcript position and composer draft.

## V07: Controls and panels

Button variants MUST retain the roles in `DESIGN.md`.
Secondary controls MUST use `tone`, `strong`, and `text`.
Quiet controls MUST have no fill or edge at rest.
Destructive controls MUST use a failed-text label and edge.
Disabled controls MUST use `tone`, `line`, and `dim`.
Every focusable control MUST draw the registered focus ring.

The common panel MUST keep its title and footer inside the available client area.
Only its body MUST scroll when it exceeds that area.
Panel widths MUST use `modalMaxWidth` and `space8` outer clearance.
At minimum window dimensions, clearance MUST shrink before required controls become unreachable.
A panel MUST retain its close action while an operation runs.
Closing a panel MUST follow the cancellation contract of that operation.

The component showcase Board geometry in `DESIGN.md` MUST NOT become the desktop shell.
Component colors, typography, and control geometry remain binding.

## V08: State and status band

| Word | Native treatment |
|---|---|
| Working | Point-filled live tag with breathing dot. |
| Done | Dim text. |
| Failed | Bold failed text with no fill. |
| Waiting | Dim text with an explicit waiting reason. |
| Declined | Dim item-status text. |
| Ready | Dim text for a thread awaiting a prompt. |
| Stopped | Dim text for an interrupted turn. |
| Offline | Warning text for a lost daemon connection. |

The lane phase words `Idle`, `Connecting`, `Connected`, and `Reconnecting` MUST use `dim` text.
The `Offline` lane phase word MUST use `warning-text`.
The `Incompatible` and `Failed` lane phase words MUST use `failed-text`.
Lane phase text MUST NOT become a fill.
Unknown item statuses MUST retain their server text.
Endpoint phase MUST remain distinct from thread outcome and from every other endpoint's phase.
Offline MUST NOT erase a recorded Done, Failed, or Stopped outcome.
The window title MUST use `dalgona · <thread title> · <state>`.

The header status band MUST follow the browser order.
It MUST show model, effort, approval, sandbox, folder, queue, context, and mark.
It MUST NOT add terminal-only cost text to the compact header.
Complete registered status values MUST remain available through a compact details panel.
Token counts MUST NOT appear outside the existing design exception.
The native client MUST NOT add a cost or usage dashboard.
Model MUST never be hidden.
Queue MUST appear only when follow-ups exist.
Context MUST show a percentage only when the context window is known.
A present temporary estimate of zero MUST remain authoritative until provider usage replaces it.

Above `statusCompactWidth`, the band MUST use three right-aligned rows.
Their contents MUST be model, then effort through queue, then context and mark.
At narrower widths, the band MUST use one row below the title.
It MUST apply the registered folder, queue, effort, and mark drop thresholds.
Model, context, approval, and sandbox MUST remain reachable.
An icon MUST have a textual accessible name containing its current value.

## V09: Motion and formatting

The dot MUST use a 1.8-second cycle with an opacity floor of 0.4.
The elapsed counter MUST update once each second while motion is allowed.
Finished rows MUST NOT tick.
Reduced motion MUST stop the dot and the elapsed timer.
A data update MAY refresh the displayed elapsed value under reduced motion.
Resuming motion MUST show the current elapsed value immediately.
Animations MUST request frames only while their visible content changes.

| Duration | Display |
|---|---|
| Less than one second | `<n> ms` |
| Less than one minute | `<s>s` |
| Less than one hour | `<m>m <ss>s` |
| One hour or longer | `<h>h <mm>m` |

Elapsed time MUST start from the server's turn timestamp.
The client MUST leave it empty until that timestamp arrives.
A new turn MUST NOT inherit an earlier turn's timestamp.
A resumed turn MUST use its recorded start.
Rate-limit waiting MUST display `rate limited · retrying in <m>m <ss>s · esc stops`.
The next zero wait notification MUST restore normal duration presentation.

## V10: Selection, scrolling, and content stability

Selections MUST refer to document positions rather than rendered widget instances.
Virtualization MUST preserve selection across rows outside the viewport.
Copy MUST use the text projection and line endings defined by the document contract.
Styling, wrapping, resizing, and incoming output MUST NOT silently change selected content.
The UI MUST visibly separate a new output indicator from the user's reading position.

The transcript MUST follow while its end is within `followDistance`.
A user scroll away MUST suspend following.
Incoming content MUST preserve the selected visible anchor and its offset.
Opening a different thread MUST follow the new thread's end.
Jump to latest MUST restore following and focus the latest row without a second scroll.
Displaying that action MUST NOT move focus.

## V11: Focus and keyboard ownership

The client MUST retain one focused control per window.
Modal panels MUST contain Tab and Shift+Tab navigation.
Request cards MUST take focus at their title when they become active.
Ordinary streaming updates MUST NOT move focus.
Closing a panel MUST return focus to its opener when that opener remains usable.
Otherwise focus MUST move to the composer, or transcript when the composer is disabled.
Focus restoration MUST preserve the current scroll position.

Selection and editing keys MUST belong to the focused editor before global commands.
The terminal panel MUST receive terminal input while its terminal viewport owns focus.
Global interrupt keys MUST NOT consume terminal escape sequences.
Only the focused request card MUST interpret its decision shortcuts.
Shortcuts MUST remain visible in [the key list](native-components.md#c23-key-list).

## V12: IME and text entry

The [shared input contract](../../dal/spec/input-text.md) is the authority for GUI, TUI, web, and daemon text semantics.
The native client MUST use its Unicode 18.0.0 default extended-grapheme target and UAX #29 revision 49 rules.
This is the specified conformance target, not a claim about the Unicode version bundled with Gio or an operating system.
Every editable component MUST preserve accepted source text and reject invalid or oversized candidates atomically.
Secret and structured fields MUST validate their declared grammar without rewriting values.

Every text editor MUST support native composition, selection, clipboard, and caret movement.
The platform adapter MUST own preedit and the complete start, update, commit, and cancellation transaction.
An active composition MUST own Enter, Escape, arrows, Tab, and candidate selection keys through the final committing event.
These keys MUST NOT send a prompt, close a panel, choose an approval, or open a command.
Composition commit MUST insert the resulting text once.
Composition cancellation MUST restore the prior committed draft and selection.
Preedit MUST NOT enter requests, persisted drafts, history, or keyword matching.
The platform work in [P05](native-platform.md#p05-finish-text-input-and-ime-integration) MUST supply missing composition ownership or event signals.
Gio editor methods alone MUST NOT count as evidence that this gate passes.
The candidate window MUST track the caret after scrolling, window movement, and DPI changes.

The client MUST preserve grapheme boundaries when deleting or moving through displayed text.
Selection, replacement, undo, and redo MUST preserve the shared editing invariants.
Protocol byte offsets MUST be converted through explicit UTF-8 scalar boundaries and Gio rune offsets.
Keyword ranges MUST NOT be treated as rune or glyph offsets.
Painted keyword ranges MUST follow the semantic-span distinction in [C14](native-components.md#c14-keywords).
Paste MUST follow the shared newline and single-line rules and the attachment rules in [native components](native-components.md#c12-attachment-chips).
Korean, Japanese, and Chinese IME verification MUST cover every declared desktop platform and input-engine pair.

## V13: Accessibility semantics

Every actionable control MUST expose its role, name, state, value, and applicable action through the native accessibility bridge.
Custom drawing MUST NOT substitute for those semantics.
Text alternatives MUST preserve each state that color or an icon also represents.
Text contrast MUST reach 4.5:1 against its ground.
Control edges and focus rings MUST reach 3:1.
Point ink MUST reach 4.5:1 against its fill.

High-contrast settings MUST preserve visible edges, selection cues, and focus rings.
Text scaling MUST preserve every action and label through wrapping or scrolling.
Screen readers MUST receive discrete turn and request announcements.
They MUST NOT receive every streamed token as an announcement.
The [platform contract](native-platform.md) defines the required operating-system bridges.
The [verification contract](native-verification.md) defines the assistive-technology checks.

## V14: Product marks and assets

The client MUST use [dalgona.svg](../../../assets/brand/dalgona.svg) at 24 logical pixels and above.
It MUST use [dalgona-small.svg](../../../assets/brand/dalgona-small.svg) below that size.
The candy face MUST retain `#DA702C`.
Its rim MUST retain `#9D4310`.
Its registered highlight MUST retain `#FFFCF0` at 45 percent opacity.
The highlight MUST remain the existing flat SVG shape.
Other components MUST NOT copy that treatment.

The mark's accessible description MUST name the caramel candy, crescent moon, and rabbit Gona.
The interface MUST NOT add explanatory rabbit prose.
Decorative repetitions MUST be hidden from accessibility traversal.
The app icon and installer assets MUST derive from the committed mark.

## V15: Rendering and frame ownership

The UI event loop MUST own Gio widget and layout state.
RPC, decoding, filesystem operations, document parsing, and figure compilation MUST execute outside the frame's blocking path.
Completed background work MUST publish bounded messages with identity and generation checks.
The [architecture contract](native-architecture.md) defines those ownership boundaries.
The [protocol contract](native-protocol.md) defines ordering and cancellation.

A frame MUST layout visible content and the bounded overscan required by [native documents](native-documents.md).
It MUST NOT reparse the whole transcript after each token.
Unchanged text shaping and completed document blocks MUST be reused.
An idle window MUST stop requesting animation frames.
Native resource failures MUST produce a readable error without replacing content with an empty surface.

## V16: Design acceptance

The implementation MUST satisfy all V01 through V16 requirements before completion.
Verification MUST cover every theme, compact layout, large text, high contrast, reduced motion, and keyboard path.
It MUST inspect merged endpoint lanes, all seven phase words, and the absence of per-endpoint fills.
It MUST cover the shared Unicode corpus, Korean, Japanese, and Chinese IME transactions, emoji, and mixed-script content.
It MUST cover long paths, large diffs, pending requests, and disconnected states.

Pixel comparisons MUST use registered tokens and known font assets.
They MUST NOT replace behavior or accessibility checks.
Source parity MUST be checked against [the browser contract](web.md) and the component inventory.
Any intentional difference MUST be stated in this specification before implementation changes it.
