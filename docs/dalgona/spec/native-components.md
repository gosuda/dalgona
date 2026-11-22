# Native components

Status: Required design. This document does not describe an implemented or certified desktop client.
The source baseline is `2c036c60b379e7718255a23ebd2c5079d76ec50c`.

The [native decision](../decisions/native-client.md) selects Gio.
The [overview](native.md) defines the complete desktop deliverable.
The [design contract](native-design.md) defines presentation and accessibility.
The [architecture contract](native-architecture.md) defines ownership.
The [protocol contract](native-protocol.md) defines existing and required server methods.
The [shared input contract](../../dal/spec/input-text.md) defines text preservation, Unicode boundaries, composition, validation, and limits for every editable component.

Every component below MUST work through the supported native transports.
A required control MUST NOT terminate at an empty panel, unsupported placeholder, or command-line handoff.
An older daemon MAY lack a required capability.
That mismatch MUST name the unavailable operation and required daemon upgrade.
It MUST NOT count as feature completion.

## Source inventory

The following table maps all 43 existing browser components.
Each requirement preserves the user task rather than the Svelte implementation.
Source paths identify the audited reference, not reusable Gio widgets.

| ID | Browser component | Required native surface |
|---|---|---|
| C01 | [App](../../../web/src/lib/components/App.svelte) | Window, workspace, connection, and panel routing. |
| C02 | [ThreadList](../../../web/src/lib/components/ThreadList.svelte) | Paged thread navigation. |
| C03 | [Transcript](../../../web/src/lib/components/Transcript.svelte) | Stable, virtualized conversation. |
| C04 | [StateTag](../../../web/src/lib/components/StateTag.svelte) | Thread and item state labels. |
| C05 | [StatusBand](../../../web/src/lib/components/StatusBand.svelte) | Compact model and policy status. |
| C06 | [Mark](../../../web/src/lib/components/Mark.svelte) | Product mark. |
| C07 | [NoticeStack](../../../web/src/lib/components/NoticeStack.svelte) | Bounded notices and recovery actions. |
| C08 | [DesktopNotifications](../../../web/src/lib/components/DesktopNotifications.svelte) | Native desktop attention. |
| C09 | [Composer](../../../web/src/lib/components/Composer.svelte) | Prompt, steering, recall, and shell input. |
| C10 | [CommandPalette](../../../web/src/lib/components/CommandPalette.svelte) | Slash-command selection. |
| C11 | [CompletionList](../../../web/src/lib/components/CompletionList.svelte) | Workspace file completion. |
| C12 | [AttachmentChips](../../../web/src/lib/components/AttachmentChips.svelte) | Pasted text and image attachments. |
| C13 | [FollowupQueue](../../../web/src/lib/components/FollowupQueue.svelte) | Queued prompt editing and removal. |
| C14 | [KeywordText](../../../web/src/lib/components/KeywordText.svelte) | Keyword ranges and summaries. |
| C15 | [ApprovalCard](../../../web/src/lib/components/ApprovalCard.svelte) | Exact command and file-change decisions. |
| C16 | [QuestionCard](../../../web/src/lib/components/QuestionCard.svelte) | Structured user answers. |
| C17 | [Markdown](../../../web/src/lib/components/Markdown.svelte) | Prose, code, figures, math, and images. |
| C18 | [DiffCard](../../../web/src/lib/components/DiffCard.svelte) | File-change diffs. |
| C19 | [ReviewView](../../../web/src/lib/components/ReviewView.svelte) | Complete output viewer. |
| C20 | [Panel](../../../web/src/lib/components/panels/Panel.svelte) | Common modal shell. |
| C21 | [ThreadPanels](../../../web/src/lib/components/panels/ThreadPanels.svelte) | Thread-action routing and guards. |
| C22 | [TurnList](../../../web/src/lib/components/panels/TurnList.svelte) | Turn selection and active markers. |
| C23 | [KeyListPanel](../../../web/src/lib/components/panels/KeyListPanel.svelte) | Contextual keyboard reference. |
| C24 | [ModelPanel](../../../web/src/lib/components/panels/ModelPanel.svelte) | Model, preset, and effort selection. |
| C25 | [SettingsPanel](../../../web/src/lib/components/panels/SettingsPanel.svelte) | Preferences and notification state. |
| C26 | [LoginPanel](../../../web/src/lib/components/panels/LoginPanel.svelte) | Provider authentication and logout. |
| C27 | [ResumePanel](../../../web/src/lib/components/panels/ResumePanel.svelte) | Thread filtering and conversation search. |
| C28 | [NewThreadPanel](../../../web/src/lib/components/panels/NewThreadPanel.svelte) | Model and working-directory selection. |
| C29 | [CompactPanel](../../../web/src/lib/components/panels/CompactPanel.svelte) | Confirmed context compaction. |
| C30 | [ForkPanel](../../../web/src/lib/components/panels/ForkPanel.svelte) | Fork from a selected turn. |
| C31 | [TreePanel](../../../web/src/lib/components/panels/TreePanel.svelte) | Branch navigation. |
| C32 | [RewindPanel](../../../web/src/lib/components/panels/RewindPanel.svelte) | Confirmed conversation rewind. |
| C33 | [AdvisorPanel](../../../web/src/lib/components/panels/AdvisorPanel.svelte) | Advisor state and controls. |
| C34 | [ReviewStartPanel](../../../web/src/lib/components/panels/ReviewStartPanel.svelte) | Change-review target selection. |
| C35 | [MessageRow](../../../web/src/lib/components/rows/MessageRow.svelte) | User and assistant messages. |
| C36 | [ThinkingRow](../../../web/src/lib/components/rows/ThinkingRow.svelte) | Reasoning display. |
| C37 | [TextRow](../../../web/src/lib/components/rows/TextRow.svelte) | Plain and otherwise unclassified items. |
| C38 | [CommandCard](../../../web/src/lib/components/rows/CommandCard.svelte) | Command, exit, duration, and output. |
| C39 | [ToolCard](../../../web/src/lib/components/rows/ToolCard.svelte) | Tool arguments and results. |
| C40 | [FileChangeRow](../../../web/src/lib/components/rows/FileChangeRow.svelte) | File changes and live review notices. |
| C41 | [AdvisorNote](../../../web/src/lib/components/rows/AdvisorNote.svelte) | Advisor severity and note. |
| C42 | [PlanPanel](../../../web/src/lib/components/rows/PlanPanel.svelte) | Current-turn plan. |
| C43 | [SubagentBlock](../../../web/src/lib/components/rows/SubagentBlock.svelte) | Child-thread state and navigation. |

C44 through C51 define required native additions.
They MUST reuse the existing shell, common panel, notices, and field treatments.

## C01: Application shell

The shell MUST contain thread navigation, a workspace header, a transcript, and a composer dock.
The dock MUST contain notices, the active request, and the composer.
The shell MUST follow the geometry and one-point rule in [native design](native-design.md).
Auxiliary features MUST open compact panels.
They MUST NOT add permanent dashboard columns.
The visible thread title MUST use at most two lines.
Its complete value MUST remain selectable and available to accessibility.

A launch without saved thread references MUST NOT open a thread automatically.
An ordinary restart MAY restore explicit references under [the restore contract](native-architecture.md#a10-restore-close-and-shared-state).
Restoration MUST NOT submit input or repeat a mutation.
The empty workspace MUST say `Pick a thread or start a new one`.
When connected, it MUST explain that earlier threads appear in the list.
When disconnected, it MUST offer connection recovery.
Start thread MUST remain focusable while its operation runs and expose its busy state.
An accessibility action MUST move directly to the transcript without changing its scroll position.

Each window MUST have a stable `windowId` and independent editing and navigation state.
Draft and follow-up keys MUST include endpoint identity, `windowId`, and `threadId`.
The application state owner MUST store those keyed payloads.
The shared worker MUST own protocol projections rather than editor widgets.
Each window's Gio event loop MUST own its editors, selection, focus, panels, and layout state.
Creating another window MUST NOT alias an existing window's mutable draft or queue.
Window-scoped asynchronous results MUST carry `windowId`, the connection generation, and the relevant thread identity.
An obsolete result MUST NOT replace the current window state.
Closing a panel MUST preserve the composer draft and transcript anchor.

Reconnection MUST follow the shared client lifecycle in [native architecture](native-architecture.md).
It MUST reload thread navigation and resume the open thread.
It MUST retain queued follow-ups for that same thread.
It MUST NOT repeat a running turn or retry an uncertain write automatically.
A resume error identifying an unknown thread MUST clear that open thread and its transcript.
Requests cached from a disconnected connection MUST lose their actionable state.
Only requests delivered by the current connection MAY become actionable again.

The client MUST treat `seq` according to its per-thread scope in [native protocol](native-protocol.md).
It MUST NOT treat `seq` as a global cursor for every notification.
Closing or switching a thread MUST release only that view's subscription interest.
The application MUST count interests by endpoint and thread.
It MUST call `thread/unsubscribe` only when the final interest ends.
One window closing MUST preserve another window's live view and pending requests.
Subscription saturation MUST offer closing an existing view or cancelling the new open.
It MUST NOT evict another window's live subscription silently.
The UI MUST preserve pending-request ownership when multiple windows or clients watch a thread.

## C02: Thread list

The list MUST show each thread's title, preview, model, update time, and state when available.
Title fallback MUST use the preview, then `Untitled thread`.
The selected thread MUST have an inline-start bar and a strong title.
Its selection MUST remain identifiable under high contrast.

Active navigation requests MUST use `thread/list` with `limit: 100` and its cursor only.
It MUST NOT send `sortKey` or `sortDirection`.
Load more threads MUST append unseen threads in response order.
Paging MUST preserve the open thread and existing rows.
Load more threads MUST remain focusable while loading and expose its busy state.

Overlapping pages MUST trigger earlier-page reconciliation under [the web contract](web.md#empty-states-and-errors).
A final non-overlapping page MUST check the first page's identifiers and update times.
A changed first page MUST trigger reconciliation of the other earlier pages.
A failed page MUST retain existing rows and retry its original cursor.
Retry MUST remove that page's previous failure notice.

A disconnected list MUST say `Threads unavailable`.
It MUST NOT claim that no threads exist.
An empty connected list MUST offer Start thread.
Opening a different thread MUST use the explicit queued-follow-up decision in C13.
A disappearing focused row MUST return focus to the composer or transcript.
Thread management MUST use C44 without adding permanent row clutter.

## C03: Transcript

Transcript identity MUST include `threadId`, `turnId`, and `itemId`.
Repeated item identifiers in different turns MUST remain distinct.
Notifications MUST update only their named thread and turn.
Nested calls MUST appear directly after their owning `eval` row.
Rows MUST be selected by item kind rather than tool name.

History loading MUST use the bounded history contract in [native protocol](native-protocol.md).
Virtualization, anchor retention, selection, and full-content access MUST satisfy [native documents](native-documents.md).
The status announcement MUST say `Loading transcript` while initial history loads.
The transcript itself MUST NOT announce streamed tokens as live updates.

Following MUST remain active while the reader stays within the registered end distance.
Output arriving between layout and scroll handling MUST NOT falsely detach following.
After detachment, incoming output MUST preserve the visible anchor and its offset.
Jump to latest MUST appear whenever detached, including idle threads.
It MUST display appended items as `· N new`.
Activation MUST restore following and focus the last row without another scroll.
Showing the action MUST NOT move focus.

The transcript MUST apply these verbosity rules.

| Level | Required presentation |
|---|---|
| `high` | Show every row fully, except failed tools retain their last ten lines. |
| `medium` | Show eval summaries, compact reasoning, command tails, and bounded inline diffs. |
| `low` | Combine consecutive tool rows into one summary group. |
| Every level | A failed tool shows its last ten lines and ends its group. |

Medium eval summaries MUST include the code-line count and state.
Medium command tails MUST show ten lines and the total line count.
Medium diffs MUST show at most 60 lines per file and the omitted count.
Low summaries MUST distinguish command counts from other tool counts.
Open full text MUST recover the complete row or group through C19.
No verbosity level MAY discard stored content.

A failed turn MUST identify its error and offer sending again or starting another thread.
A retrying turn MUST identify the last error without claiming final failure.
An interrupted turn MUST say `Turn interrupted. Send a message to continue.`
Live edit-review and turn-review lines MUST follow C40.
History MUST NOT invent those live verdicts.

## C04: State tag

State words and their exact treatments MUST follow [native design](native-design.md#v08-state-and-status-band).
The header MUST distinguish Working, Waiting, Done, Stopped, Failed, Ready, and Offline.
Waiting MUST include a reason when one is known.
Item state MUST also distinguish Declined and Timed out.
Unknown item states MUST retain the server's text.

Thread status `notLoaded` MUST map to idle.
An idle thread without a known completed turn MUST display Ready.
A repeated start notification MUST NOT overwrite a completed outcome or its error.
Offline MUST preserve the recorded outcome separately.
Rate-limit waiting, elapsed time, and reduced motion MUST follow the design contract.

## C05: Status band

The band MUST retain model, effort, approval, sandbox, folder, queue, context, and mark in that order.
It MUST respect `statusHide`, except model MUST remain visible.
Activating the model MUST open C24.
Activating another status value MUST open a compact details view containing its complete value.
That view MUST make hidden-by-width values available without widening the shell.

Effort MUST use its rank among the selected model's efforts.
It MUST disappear when fewer than two efforts exist.
The effective effort MUST use the thread setting, then the model default.
Sandbox `off` MUST use the registered none glyph.
Folder MUST expose the working directory as selectable text in details.
Queue MUST show a count only when nonempty.

Context MUST use the moon gauge and percentage when the context window is known.
The compact band MUST NOT add token counts or terminal-only cost text.
Details MUST show only the registered status values.
They MUST NOT introduce token counts or a cost dashboard.
A present temporary estimate MUST take precedence, including zero.
Reported provider usage MUST replace that temporary estimate.
Responsive drops and textual icon names MUST follow the design contract.

## C06: Product mark

The mark MUST use the committed product assets and registered size variants.
Its semantic description and brand colors MUST follow [native design](native-design.md#v14-product-marks-and-assets).
A decorative mark MUST NOT create an extra keyboard stop.

## C07: Notice stack

The shell MUST provide one error announcement channel and one status announcement channel before notices arrive.
The stack MUST hold at most five notices.
Identical notice text MUST replace its earlier instance.
Overflow MUST remove the oldest notice first.
Each notice MUST name only controls it actually offers.
Dismiss MUST remove the notice and return focus to the composer or transcript.

A failed operation MUST show its action and specific cause.
While connected, a safe retry MUST repeat the same operation parameters.
While disconnected, the notice MUST offer Reconnect.
An uncertain write MUST follow the explicit reconciliation path in [native protocol](native-protocol.md).
Its retry MUST NOT be presented as harmless repetition.

Connection messages MUST name the selected local or remote endpoint.
They MUST NOT instruct native users to run `dalgona web`.
Automatic retries MUST use the bounded shared lifecycle policy.
Manual Reconnect MUST cancel a scheduled attempt and connect immediately.
The header MUST remain Offline until the connection succeeds.
Late transport errors MUST NOT change the current connection presentation.
Already dispatched mutations MUST retain their unknown-outcome records across that transition.

Panel operation errors MUST remain inside their panel unless C24 or C25 specifies a save notice.
Composer errors MUST appear beside the field and identify its accessible error description.
Search progress MUST remain in its panel.
Pending requests from other threads MUST share one notice with Open thread.

## C08: Desktop notifications

Notifications MUST require explicit user opt-in and operating-system permission.
The client MUST NOT request permission during launch.
It MUST notify only when no window showing that endpoint and thread is visible and active.
Waiting requests MUST use `Waiting for your reply`.
Completed, failed, and interrupted turns MUST use Done, Failed, and Stopped.
Each notification MUST identify its thread and avoid including transcript content.

Notification identity MUST include endpoint and thread identity.
Duplicate delivery across reconnects MUST NOT produce duplicate attention for the same event.
Activation MUST focus the appropriate window and open the named thread.
It MUST NOT send input or approve a request.
The Settings panel MUST show on, off, blocked, or unavailable state.
A blocked permission MUST offer the platform's notification settings when available.
The [platform contract](native-platform.md) defines platform delivery and activation.

## C09: Composer

The composer MUST retain its visible label, editable field, help line, and action area.
Its primary control MUST retain focus identity while its label changes.
Every editor MUST follow the IME and accessibility rules in [native design](native-design.md).
Each committed edit MUST be atomic and preserve the accepted scalar sequence under the shared input contract.
Caret movement, selection, replacement, Backspace, Delete, and word deletion MUST end at extended grapheme boundaries.
Edits MUST repair affected boundaries when new scalars join adjacent clusters.
One paste, accepted completion, or composition commit MUST form one undoable transaction.
Undo and redo MUST restore exact text and selection without splitting clusters.
Preedit MUST remain adapter-owned and MUST NOT enter sends, history, keyword requests, or completion searches.
An invalid or oversized candidate MUST remain recoverable without modifying committed text or sending a prefix.

| Key | Context | Required effect |
|---|---|---|
| Enter | Idle thread | Send the complete message. |
| Enter | Running turn | Steer the current turn with typed text. |
| Shift+Enter | Message field | Insert a newline. |
| Alt+Enter | Running turn | Queue typed text as a follow-up. |
| ArrowUp | Start, without selection | Recall the previous sent message. |
| ArrowDown | End or recall browsing, without selection | Recall the next message or restore the draft. |
| Escape | Running turn | Arm the two-second interrupt guard. |
| Escape twice | Within that guard | Interrupt the turn. |
| Escape | Rate-limit wait | Interrupt immediately. |
| Escape | Idle thread | Leave the draft unchanged. |

Recall MUST place the caret at the recalled text's start.
Editing recalled text MUST make it the current draft.
Recall MUST preserve the original draft until the user edits or sends its replacement.
Command and file-completion keys MUST take precedence when their lists own selection.
An active IME composition MUST take precedence over all actions in this table.

Send MUST combine typed text, pasted text, and images in that order.
Empty typed text MUST NOT produce an empty text item.
Text attachments MUST retain their order and boundaries in the resulting message.
Steering and follow-up queueing MUST retain attachment chips for the next complete send.
Known send failures MUST retain recoverable text and attachments.
An uncertain send MUST retain its payload and expose the protocol's outcome-reconciliation path.

### Shell input

A leading `!` MUST switch the field label to `Shell command`.
The trigger MUST be ASCII `!`. Removing that marker MUST preserve every remaining command scalar, including whitespace.
The help MUST say `Enter runs the command in <cwd>. It is not sent to the agent.`
The displayed directory MUST be the granted runner workspace root.
Enter MUST acquire or reuse workspace authority with the `terminal` right.
The draft MUST remain intact while permission is pending or declined.
Execution MUST call `dalgona/workspace/exec` with its workspace handle, exact command text, and operation identifier.
The backend MUST select the bound runner's shell, directory, and sandbox.
The GUI MUST NOT send a thread-runner path to host `command/exec`.
It MUST NOT substitute a local desktop shell when remote execution is unavailable.
This runner-aware execution deliberately replaces the browser's daemon-host shell path.
The timeout MUST be 120 seconds and retained output MUST be bounded to 64 KiB.
Command text MUST fit the protocol's 64 KiB UTF-8 limit.
An empty command MUST say `Enter a command after !.`
Alt+Enter MUST say `Shell commands run with Enter` and MUST NOT queue it.

Without an open thread and known directory, the draft MUST remain with `Open a thread before running a shell command`.
An `exited` result MUST add one transient transcript row and one output chip, including a nonzero exit.
The chip MUST use `[Shell #n exit N, L lines]`.
The next complete message MUST include its text as another text block.
The generated output fence MUST exceed every backtick run in its content and contain at least three backticks.
A failed RPC MUST show a failed row and MUST NOT create an output chip.
Timed-out or cancelled execution MUST identify its termination and preserve retained partial output in its row.
It MUST NOT create a chip claiming ordinary process completion.
Truncated output MUST identify the retained limit in the row and its chip when present.
Unknown execution outcomes MUST use receipt reconciliation before any explicit repeat.
A result for another thread MUST NOT enter the current thread's row or chips.
Transient shell rows MUST NOT be presented as journaled history.
Interactive terminals MUST use C49 rather than this one-shot command path.

## C10: Command palette

A leading `/` MUST open prefix-matching commands.
Arrow keys MUST move through the visible matches.
Enter or Tab MUST open the selected panel and clear the command text.
Escape MUST close the list and preserve the field.
Recall and follow-up editing MUST reopen matching commands.

Text after a space MUST hide command matches.
Enter in that state MUST preserve the field and open no panel.
A leading `//` MUST lose one slash on send, steer, and queue paths.
A second slash inside the first word MUST make the input ordinary text.
For example, `/etc/hosts` MUST remain message text.
An unknown command MUST show its name and explain the `//` escape.
It MUST NOT send a message.

The palette MUST retain these existing commands.

| Command | Required destination |
|---|---|
| `/model` | C24 model selection. |
| `/settings` | C25 settings. |
| `/login` | C26 provider authentication. |
| `/resume` | C27 thread search. |
| `/new` | C28 new thread. |
| `/compact` | C29 context compaction. |
| `/fork` | C30 turn fork. |
| `/tree` | C31 branch navigation. |
| `/revert` | C32 rewind. |
| `/advisor` | C33 advisor controls. |
| `/review` | C34 change review. |
| `/help` | C23 keyboard reference. |

The native client MUST add these compact panel commands.

| Command | User task |
|---|---|
| `/remote` | Select and manage daemon endpoints through C45. |
| `/runner` | Inspect runner mode and transfer files through C46. |
| `/mcp` | Configure and authenticate MCP servers through C47. |
| `/files` | Browse and edit workspace files through C48. |
| `/terminal` | Open or attach a real terminal through C49. |
| `/doctor` | Run selected diagnostics through C50. |
| `/failures` | Inspect failure records and reporting consent through C51. |

The same destinations MUST be available through a compact application menu.
Disabled actions MUST state their unmet prerequisite.
Commands MUST NOT bypass the corresponding panel's validation or confirmation.

## C11: File completion

An `@` at a word boundary MUST start file completion for the following non-whitespace text.
The trigger MUST be ASCII `@`. Unicode lookalikes MUST remain ordinary text.
The query MUST wait 120 milliseconds after the latest keystroke.
An empty query MUST send no request.
The only search root MUST be the open thread's authoritative working directory.
The client MUST obtain it from the top-level `cwd` in start or resume results.
No known directory means no search and no file list.
Completion MUST acquire or reuse the thread's read-only workspace authority under [native protocol](native-protocol.md).
The directory displayed for that grant MUST use the runner's authoritative root mapping.
Missing authority MUST remain a visible permission state rather than an empty successful search.

The list MUST show at most 20 files with name and path.
Arrow keys MUST remain within those rows.
Enter or Tab MUST insert `@<path>` and a trailing space without sending.
Query edits MUST clear old results immediately.
Enter or Tab during a pending current query MUST NOT send.
Caret movement inside the same word MUST NOT trigger another search.
Escape MUST close the list and preserve the field.
Results from an old query, thread, directory, or connection MUST be discarded.
Replacement MUST also match the field, draft revision, selection range, and composition transaction that requested the result.
An unchanged query string alone MUST NOT authorize replacement after another edit or selection change.
Wire spans MUST use UTF-8 byte offsets. Gio selection offsets MUST use runes, with explicit validated conversion between them.
Replacement endpoints MUST be committed extended grapheme boundaries. Completion MUST NOT modify active preedit.
The inserted path MUST retain the selected result's exact scalar sequence and form one undoable edit.
Completion MUST use `dalgona/workspace/search` with its workspace handle.
Incomplete searches MUST identify partial results without claiming exhaustive matches.
The client MUST NOT substitute host `fuzzyFileSearch` for runner-aware completion.

## C12: Attachment chips

Pastes containing at least ten lines MUST become text chips.
Line counting MUST follow the shared line-ending conversion. Chip text MUST preserve every accepted scalar, including the final LF.
Chip identities MUST remain separate from Unicode text and MUST NOT reserve private-use scalars or noncharacters.
Each chip MUST expose its kind, useful size or line count, Show, and Remove actions.
Image input MUST support file selection, clipboard paste, and drag-and-drop through [native platform](native-platform.md).
Each image MUST be PNG, JPEG, or GIF and at most 5 MiB.
The draft MUST contain at most ten images.
Combined image-file bytes MUST remain within 10 MiB after base64 decoding.
This transport limit MUST NOT be interpreted as a decoded-pixel memory budget.
Pixel-memory limits MUST follow [native documents](native-documents.md).

A model without image input MUST reject a new image with `this model does not accept images`.
Existing draft text MUST remain intact.
Changing to an incompatible model MUST keep attached images and block sending with a visible correction action.
The input array MUST remain within 15 MiB of JSON.
That budget MUST count UTF-8 bytes of the fully serialized input array, including JSON escapes and expanded chips.
The complete request MUST remain within the transport's 16 MiB frame limit.
Oversized input MUST remain editable and MUST NOT be sent.

Text and image ordering MUST remain stable during editing, upload preparation, and retries.
Steering and queueing MUST retain chips.
Successful complete sends MUST clear only the exact submitted draft generation.
Late success MUST NOT clear newly typed input or newly attached files.

Recorded user images MUST load through `dalgona/item/image/read`.
The client MUST NOT infer local paths from `dalgona-blob:` references.
Image display, bounded decoding, previews, copy, and export MUST follow [native documents](native-documents.md).
Unsupported or damaged images MUST retain their label and error instead of becoming blank space.

## C13: Follow-up queue

The queue MUST show each item's first line above the field.
Each item MUST have Edit and Remove actions.
Edit MUST remove that queued item and place its complete text in the field.
It MUST reopen the command list when applicable.
Remove MUST affect only its selected item.
Queue order MUST remain stable.
Each window's queue MUST contain at most 32 items and 256 KiB of UTF-8 text.
Exceeding either bound MUST preserve the field and show a queue-full error.

The client MUST send the next item only after the shared reducer permits progression.
It MUST remove that item from the queue before starting its send.
A failed send MUST retain the payload in an actionable failure notice.
An explicit safe retry MUST send that payload once.
An uncertain result MUST require reconciliation before any repeat.
Reconnect MUST retain the remaining queue for the same thread.
Switching threads with queued follow-ups MUST show their count and request an explicit discard decision.
The actions MUST be Discard follow-ups and open, and Cancel.
Cancel MUST preserve the current thread and queue.
Confirmed switching MUST discard the old queue only after the new thread opens successfully.
A failed open MUST retain the original queue.
The status-band count MUST match the remaining queue.

## C14: Keywords

User messages MUST underline every valid server-reported keyword range.
They MUST also list keyword words on a `Keywords:` line.
The composer MUST show draft keywords as words on the same label.
The client MUST use the protocol's byte-offset definition.
It MUST validate bounds, ordering, source revision, and UTF-8 scalar boundaries before converting ranges into Gio rune offsets.
A valid semantic span MAY end inside a grapheme when its endpoint remains a scalar boundary.
Only its painted underline MUST expand to complete surrounding graphemes.
The expansion MUST NOT change semantic byte ranges, keyword values, copied text, or edit selection.
Malformed or stale metadata MUST leave the original text visible without marks.
It MUST NOT corrupt text, introduce replacement characters, or split a UTF-8 sequence.
Underlines MUST remain visible in high contrast and while text is selected.

## C15: Approval card

Approvals, questions, sandbox choices, and workspace permissions MUST share one arrival order.
The dock MUST show the oldest relevant request for the open thread or its child.
Other-thread requests MUST produce the notice defined in C07.
Each request MUST retain its original request identifier and source thread.
Each connection MUST deduplicate repeated delivery of that identifier.
Native request manifests MUST load through the bounded detail reads in [native protocol](native-protocol.md).
The worker MUST reconstruct and validate the exact original parameters before publishing an actionable card.
Decision controls MUST remain disabled until all required details and their digest have been verified.
Loading failures MUST retain request identity and expose a retry without assuming approval.
Resolution or timeout during loading MUST cancel the obsolete reads.

The card MUST show `Approval required`, its position when queued, and the exact request details.
Command approvals MUST ask `Run this command?`.
File approvals MUST ask `Apply these changes?` and show their supplied diffs.
Command text MUST remain verbatim rather than reconstructed from arguments.
The card MUST show the working directory, reason, escalation state, and child identity when supplied.

Command and file approvals MUST offer Accept, Accept for thread, Decline, and Cancel and interrupt.
Accept for thread MUST send the existing wire decision `acceptForSession`.
Its explanation MUST identify the matching thread grant and persistence described in [approvals](../../dal/spec/approvals.md#thread-grants).
It MUST NOT promise blanket approval for every future action.
This native wording intentionally replaces the browser's session terminology.

While a command or file approval owns focus, `y`, `a`, and `n` MUST invoke its corresponding decision.
Escape in those cards MUST cancel and interrupt immediately.
The composer interrupt guard MUST NOT apply to a focused request.
Editable fields and active IME composition MUST retain their keys.
Only one decision MAY be submitted from that card while a response is pending.

The header and actions MUST remain reachable outside the scrolling details region.
The composer MUST yield height down to its label and one field row before shrinking request details.
When necessary, the dock MUST scroll without losing any action.
The details region MUST NOT create its own Tab stop.
Title or action focus MUST route arrows, PageUp, PageDown, Home, and End to details scrolling.
The arrow step MUST be 48 dp.
The page step MUST equal details height minus 48 dp, with a 48-dp minimum.
The card MUST take title focus on arrival and restore focus when resolved.

Sandbox choice MUST remain a separate request type.
It MUST show the missing-sandbox cause and distinct Accept and Dangerously accept choices.
It MUST explain their thread-level execution consequences from [approvals](../../dal/spec/approvals.md#gate-order).
It MUST NOT substitute an ordinary approval or silently choose unconfined execution.

Workspace MCP trust MUST show the workspace path, commands, arguments, and environment key names supplied by the host.
It MUST NOT reveal environment secret values.
The choice MUST follow [MCP workspace trust](mcp.md#workspace-trust) through the same ordered request queue.
Opening a thread or inspecting the MCP panel MUST NOT manufacture a trust request.

`dalgona/workspace/requestPermission` MUST show its thread, runner, binding, root, requested rights, and reason.
Its title MUST ask `Allow workspace access?`.
Its Allow once and Decline actions MUST send `allowOnce` and `decline` respectively.
The card MUST explain raw file access and command execution when those rights are requested.
The explanation MUST state that access applies to the requested handle until expiry or revocation.
It MUST NOT offer a thread grant or reuse an agent tool approval.
Escape MUST decline this workspace grant without interrupting an unrelated turn.
Timeout, cancellation, and runner changes MUST close the permission card under the protocol contract.

The first server-accepted answer MUST win across all clients.
`serverRequest/resolved` MUST remove the matching card and cancel its pending handler.
A resolution by another client MUST produce an informative notice.
Disconnect MUST remove stale actionable requests.
The UI MUST NOT invent replay after the last subscriber's departure resolves a request.

## C16: Question card

The card MUST show `Question required`, queue position, each header, and each question.
The first option MUST carry Recommended.
Single-selection questions MUST permit one choice.
`multiSelect` questions MUST permit multiple choices.
`isOther` MUST add a text field.
A question without options MUST show only its text field.
Selected option previews MUST appear in monospace below the options.

Submit MUST send original option labels in their declared order, followed by accepted typed text without trimming or normalization.
Display escaping MUST NOT change a selected label or identifier.
Typed text MUST fit the shared 2000 Unicode-scalar budget.
The complete candidate MUST pass validation. No edit, paste, composition commit, or submit MAY silently truncate it.
Rejection MUST retain the previous committed value and keep the complete candidate recoverable with a field error.
Skip MUST send every question with an empty answer list.
Dismiss and Escape MUST send `{"answers": {}}`.
The card MUST preserve entered answers until `serverRequest/resolved` confirms resolution.
Local submission MUST NOT count as daemon acceptance.
A correlated validation warning MUST restore editing and identify the invalid field when supplied.
The original request ID and deadline MUST remain unchanged.
Correction and retry MUST reuse that pending request. They MUST NOT create another question or consume a successful-answer slot.

The shared request layout, focus, scrolling, and IME rules from C15 MUST apply.
Submit MUST be the card's sole point-filled action.
`serverRequest/resolved` MUST remove the matching card and announce its resolution.
A timeout MUST announce `Question timed out` and label its transcript item Timed out.
Late replies MUST NOT reactivate the request or create another answer.

## C17: Rich documents

The native document renderer MUST satisfy [native documents](native-documents.md) completely.
It MUST provide selectable prose, code, lists, tables, quotations, and links.
It MUST render required Mermaid, D2, nomnoml, SVG, and mathematical content natively.
It MUST display recorded user images through the image RPC.
These requirements MUST NOT depend on a WebView, browser DOM, or Electron renderer.

Markdown headings MUST shift down two levels, capped at level six.
Raw HTML MUST remain literal text.
Links MUST retain only HTTP, HTTPS, and mailto targets.
Opening a link MUST use the platform handler after a user action.
Other link schemes MUST retain their visible text without activation.
Markdown remote images MUST show `image: <alternative text>` without automatic network requests.
Recorded user attachments MUST remain distinct from remote Markdown images.

Syntax languages and aliases MUST match the document contract.
Figures MUST expose source, accessible description, copy, and export actions.
An invalid figure MUST retain its source and a specific error.
Malformed input MUST NOT become an executable surface or an empty success.
Full rendering support MUST be verified before the feature counts as complete.

## C18: Diff card

Every file change MUST show its path and Added, Changed, or Deleted.
Reported addition and deletion counts MUST appear as text.
Each added and removed line MUST retain its prefix and separate visual role.
Color MUST supplement those textual distinctions.
Inline diff lines MUST NOT create an inner scroll container.
Long content MUST follow the document wrapping and full-view rules.

An omitted server diff MUST say `Diff not shown.`
Verbosity truncation MUST state the omitted line count and offer the full viewer.
An approval MUST display its supplied diffs before any decision.
Absent content MUST NOT be reconstructed by reading an unrelated local workspace.
Full diffs MUST remain selectable and exportable under C19.

## C19: Full output review

Open full text MUST open the complete selected row or tool group.
Ctrl+O from the composer or transcript MUST open the latest finished row.
The viewer MUST retain its opener and restore focus on Escape or Close.
It MUST provide text selection, copy, search, and export for its full logical document.
Large content MUST remain demand-paged and bounded in memory.

Spilled output MUST load through snapshot-bound `dalgona/history/content/read` references one page at a time.
Those references MUST identify the exact turn and complete masked spill under the native protocol contract.
The viewer MUST NOT fetch every page before showing the first content.
Copy all and Export MUST stream through the bounded document path.
They MUST report completion or failure without silently exporting a truncated tail.
Paging failures MUST retain already loaded content and retry the same cursor.
Thread, item, or connection changes MUST cancel obsolete reads.
The viewer MUST distinguish missing content from empty output.

## C20: Common panel

Every modal panel MUST use one title, scrollable body, and reachable footer.
Only the body MUST scroll when the panel exceeds the client area.
Tab and Shift+Tab MUST remain inside the active panel.
Escape MUST close the panel unless an active composition owns it.
Every state MUST retain an explicit Close or Cancel action.
Busy controls MUST expose progress and reject duplicate activation.

Closing MUST return focus to the usable opener, otherwise the composer or transcript.
Focus restoration MUST preserve scroll position.
Each operation MUST declare whether closing cancels work or merely stops observing it.
A closed panel's result MUST NOT mutate a newly opened panel.
Completed daemon work MUST remain discoverable after the panel closes.
Field values MUST survive recoverable validation errors.
Secret fields MUST clear when their authentication flow ends.

## C21: Thread panel routing

Thread actions MUST resolve their target when invoked and preserve that identity during the operation.
Changing the open thread MUST NOT redirect a pending action to another thread.
While a turn runs, new, compact, fork, tree, and revert MUST refuse entry.
The refusal MUST explain the running turn and offer Interrupt and Close.
Interrupt MUST target that exact turn.
The requested action MUST require a new explicit invocation after interruption completes.

Compact, fork, tree, rewind, advisor, review, and resume errors MUST appear inside their panels.
They MUST NOT duplicate the error in the global notice stack.
The application menu and slash commands MUST share these guards.

## C22: Turn list

Turn lists MUST retain server order and turn identifiers.
Each row MUST show its preview or `Untitled turn`.
The active turn MUST carry an Active text marker.
Selection MUST use accessible single-choice semantics and keyboard navigation.
Empty lists MUST explain the unavailable action.
Loading MUST preserve a valid selection when the same turn remains available.
Confirmation MUST revalidate the selected identifier against the current result.

## C23: Key list

The key list MUST be available through `/help`, the application menu, and `?` outside editable controls.
It MUST show the shortcuts actually supported by the focused context.
Platform-specific editing shortcuts MUST use the platform's labels.
The documented control shortcuts below MUST remain available on every supported platform.

| Context | Keys | Action |
|---|---|---|
| Composer | Enter, Shift+Enter, Alt+Enter | Send or steer, newline, queue. |
| Composer | ArrowUp, ArrowDown | Recall at the documented caret positions. |
| Composer | Escape twice | Interrupt within two seconds. |
| Rate-limit wait | Escape | Interrupt immediately. |
| Command or completion list | Arrows, Enter, Tab, Escape | Select, accept, or close. |
| Command or file approval | `y`, `a`, `n`, Escape | Accept, thread grant, decline, cancel and interrupt. |
| Workspace permission | Escape | Decline the requested workspace access. |
| Question | Escape | Dismiss with an empty answers object. |
| Request title or actions | Arrows, PageUp, PageDown, Home, End | Scroll request details. |
| Transcript or composer | Ctrl+O | Open the latest finished row. |
| Modal or full viewer | Escape | Close and restore focus. |
| All dialogs | Tab, Shift+Tab | Traverse contained controls. |

The list MUST explain that IME composition owns its candidate-selection keys.
It MUST explain that terminal focus sends terminal input instead of composer shortcuts.
Shortcut labels MUST remain accurate when platform modifiers differ for ordinary editing.

## C24: Model panel

The panel MUST list every visible model with its description.
Hidden models MUST NOT appear as selectable choices.
Presets MUST show default, smol, and slow in that order when supplied.
Each preset MUST name its model and default effort.
Selecting a preset MUST populate the model and effort together.

The model MUST exist in the current server list.
The effort MUST belong to that model's declared efforts.
Changing models MUST replace an invalid effort with that model's declared default.
Saving MUST use one `config/batchWrite` for `model` and `model_reasoning_effort`.
It MUST NOT modify the open thread's approval or sandbox.
A failed save MUST retain the selections and show a save notice.
A model list without entries MUST explain authentication and offer Log in.
Model changes MUST revalidate attached images under C12.

## C25: Settings panel

Settings MUST load from `dalgona/settings/read`.
The panel MUST expose approval, verbosity, theme, status visibility, and edit review.
It MUST use the server's allowed values rather than accepting arbitrary strings.
Empty or unknown choices MUST identify the invalid field and block saving.
Save MUST send only changed paths in one `config/batchWrite`.
The paths MUST be `approval`, `verbosity`, `theme`, `statusHide`, and `editReview`.

Successful saves MUST reload settings and apply the theme immediately.
Changing approval MUST explain that open threads keep their existing approval and sandbox.
Model MUST remain unavailable as a hidden status item.
Edit review MUST explain that each edit's masked diff is sent to the decider model.
Failed saves MUST retain values and show a notice.

The final row MUST show native notification permission and user preference under C08.
Notification permission MUST remain separate from daemon configuration.
Endpoint connection, MCP configuration, and failure reporting MUST link to their dedicated compact panels.
Opening Settings MUST NOT enable reporting, authenticate, or start a network diagnostic.

## C26: Provider login

The panel MUST list providers and supported methods from `dalgona/auth/providers`.
It MUST use provider targets with the exact identifiers from that response.
API-key providers MUST offer a masked editable field and explicit submit.
Keys, authorization codes, and redirect values MUST retain their accepted text exactly.
Validation MUST apply the advertised field grammar without trimming or normalizing secrets.
Hybrid providers MUST offer their supported key and OAuth methods within this panel.
Browser and device flows MUST show their authorization URL, code when supplied, and progress.
The user MUST be able to copy the URL and code.
Manual flows MUST provide a field for the advertised redirect URL or authorization code.
Cursor flows MUST display the supplied challenge and support the advertised native application handoff.
Every flow MUST show expiry, cancellation, failure, and successful completion distinctly.

Authentication MUST use the generalized `dalgona/auth` methods in [native protocol](native-protocol.md).
An API-key flow MUST start with `flow:"apiKey"` and no `callbackMode`, then submit the protected field to the returned transaction.
It MUST NOT open a browser or allocate a callback listener.
Other flows MUST use the exact callback variant in R09. The panel MUST NOT invent a callback mode for a direct-key flow.
Every status update MUST match the flow identifier and connection generation.
Callback-based remote OAuth MUST use an offered device or manual variant when the backend callback is unreachable from the desktop.
Offered callback-free flows remain available without that fallback.
The panel MUST NOT present a remote loopback URL as a local callback listener.
The panel MUST retain Close in every state.
Closing or replacing the panel MUST cancel its outstanding flow.
A late start response MUST be cancelled using its returned identifier.
API-key failures MUST appear beside the field and keep the panel open.
OAuth failures MUST offer a fresh Try again action.

Success MUST announce the provider, refresh account state and models, and close the login flow.
An uncertain completion MUST refresh the provider descriptor before offering another credential submission.
The provider details MUST expose the current account and explicit logout.
Logout MUST refresh models and explain unavailable selections without erasing drafts.
Secrets MUST NOT enter logs, transcripts, diagnostics, or persistent GUI preferences.
No supported provider method MAY require a terminal-only handoff.
External browser authorization remains a deliberate platform action under [native platform](native-platform.md).

## C27: Resume and search

The panel MUST use C02 paging and filter title, preview, and identifier immediately.
A query MUST start conversation search after 300 milliseconds only when it contains at least one Unicode-whitespace-separated term.
Every term MUST contain at least three Unicode scalars. Padding and separators MUST NOT satisfy that minimum.
Eligibility MUST remain separate from the value. Search MUST send the unchanged query.
`ab ` and `abc de` MUST remain local filters. ` abc ` MUST search with its spaces preserved.
Derived matching keys MUST NOT replace field text, result identities, or source offsets.
Each new keystroke MUST cancel the previous search and clear its results.
Earlier-connection results MUST be discarded.
Matches in conversations MUST show each matching title and snippet.
The search MUST NOT require downloading every transcript to the desktop.

No matches MUST say `No conversation mentions this text.`
A failure MUST show `Search failed: <cause>.` and Try again.
Selecting a result MUST retain the panel until opening completes.
While opening, choices MUST be disabled and the panel MUST show `Opening thread`.
Success MUST close the panel.
Failure MUST preserve the filter and results for recovery.
If a focused choice disappears, focus MUST return to the filter.

## C28: New thread

The panel MUST expose the selected model and working directory before creation.
A local directory chooser MUST use the native platform dialog.
Remote creation MUST offer explicit working-directory path entry in the selected endpoint's path syntax.
The start operation MUST validate that path under the existing backend contract.
Runner-aware directory browsing becomes available through C48 after thread creation.
The client MUST NOT silently substitute its own local working directory for a remote path.
The panel MUST show the effective runner, approval mode, and sandbox when known.
Creation MUST call `thread/start` with validated values.
Success MUST use the authoritative returned thread and top-level directory.
Failure MUST retain the selections and provide a specific retry path.

## C29: Context compaction

The panel MUST explain that earlier turns remain in the journal.
It MUST require the explicit Compact action.
Compaction MUST use `thread/compact/start` under [native protocol](native-protocol.md).
Progress MUST remain visible and duplicate activation MUST be blocked.
Success MUST refresh the thread and context estimate without changing model or policy.
Failure MUST remain in the panel.

## C30: Fork

The panel MUST present C22 turns and require a selected turn.
Fork MUST call `thread/fork` with that exact source thread and turn.
Success MUST open the returned thread.
It MUST preserve the source thread's history.
Failure MUST retain the selected turn and source identity.

## C31: Branch tree

The panel MUST show turn previews, parent relationships, and the active leaf.
It MUST remain a compact branch-navigation surface rather than a permanent graph dashboard.
Choosing a leaf MUST call `dalgona/thread/leaf/set` with its exact turn.
Success MUST reload the resulting active path.
Failure MUST retain the previous active path and selection.
The panel MUST NOT imply that changing the leaf restores workspace files.

## C32: Rewind

The panel MUST identify the selected user turn and its preview.
It MUST explain that rewind changes the conversation path and does not revert workspace files.
Only explicit confirmation MAY call `thread/revert`.
Every user message MUST expose Rewind to here for its own turn.
Success MUST refresh the active path and context.
Failure MUST retain the target and explanation.

## C33: Advisor

The panel MUST show every advisor's name, model, and current state.
Loading, unavailable, and unattached states MUST remain distinct.
It MUST offer turning the advisor on or off and resetting it through existing advisor methods.
Actions MUST preserve their target thread and refresh the displayed state.
Failures MUST remain in the panel.
Advisor notes MUST use C41 in the transcript.

## C34: Start change review

The panel MUST offer uncommitted changes, base branch, commit, and custom instructions.
Base branch MUST require a nonempty branch.
Commit MUST require a nonempty identifier.
Custom review MUST require nonempty instructions.
Branch and commit fields MUST validate their declared grammar without rewriting the submitted value.
Custom instructions MUST preserve accepted text exactly, including leading and trailing whitespace.
Whitespace-only eligibility checks MUST NOT trim the stored or submitted instructions.
Start review MUST call `review/start` with the chosen target.
Success MUST open or follow the returned review thread as specified by the protocol.
Errors MUST remain inside the panel with all inputs intact.

## C35: Message row

User and assistant messages MUST remain visibly distinct through labels and layout.
Assistant content MUST use C17.
User content MUST retain keyword ranges, attachment order, and recorded images.
User messages MUST expose Rewind to here.
Copy MUST preserve source content without display-only labels.
Streaming MUST update the same logical message rather than append duplicate rows.
Selection MUST remain stable while later text arrives.

## C36: Thinking row

The row MUST read Thinking.
At high verbosity, it MUST remain open while reasoning streams.
At medium and low, it MUST use one summary line with its line count.
While streaming at those levels, it MUST show only the latest reasoning line.
Its Open full text action MUST recover complete available reasoning.
It MUST NOT manufacture reasoning absent from the server payload.

## C37: Text row

Plain-text items MUST preserve their full source and state.
Unknown item kinds MUST show their readable type name instead of disappearing.
Unknown structured content MUST remain available as safe text in the full viewer.
It MUST NOT be interpreted as markup or a command because its kind is unknown.
Unsupported optional fields MUST NOT corrupt adjacent known items.

## C38: Command row

Each command row MUST show `$ <command>`, state, reported exit code, and reported duration.
It MUST NOT invent an exit code or completion timestamp.
The row MUST show the verbosity-selected output and total count.
Failed output MUST retain its last ten lines and the full-view action.
Spilled output MUST remain accessible through C19.
Terminal control sequences MUST follow the safe text rules of [native documents](native-documents.md).
Command output MUST NOT execute clipboard or hyperlink control sequences.

## C39: Tool row

The row MUST show the tool name, state, duration when reported, and output.
Full presentation MUST expose structured arguments as readable text.
Collapsed and grouped presentations MUST retain Open full text.
Dynamic tools MUST display text from `contentItems`.
Hashline and snapshot marker-only lines MUST be removed under the existing output-presentation rules.
Their source MUST remain available where the document contract requires exact copying.
Advisor items MUST route to C41 through their parsed item kind.
Question timeouts MUST remain distinct from tool failures.

## C40: File-change row

Every changed file MUST render through C18.
The row MUST retain file order and supplied change kind.
Live edit review MUST appear as one line below its diff cards for every verdict.
The line MUST show the exact summary supplied by the server.
Flagged verdicts MUST use warning text.
Errors MUST use bold failed text.
Other verdicts MUST use dim text.
Review lines MUST have no filled background.

The current turn's review MUST appear after its last row.
Only the open turn MAY show that live turn review.
History MUST NOT recreate a verdict from an earlier live notification.
A nonempty review notice MUST receive one status announcement.

## C41: Advisor note

The note MUST identify the advisor, severity, and complete note text.
Severity MUST appear as a word, not only color.
The inline-start border MUST provide a second severity cue.
Concern MUST use the registered accent-text border.
Blocker MUST use the registered failed-text border.
Other notes MUST use the strong border.
The note MUST preserve line breaks and selectable text.

## C42: Plan

The transcript MUST show the latest plan for the current turn.
It MUST include the optional explanation and ordered steps.
Each step MUST read Done, Working, or Pending.
The working step MUST expose the native current-step semantic state.
A new turn MUST clear the previous live plan.
Plan updates MUST replace the plan without moving transcript focus.

## C43: Subagents

Each child of the open thread MUST appear with title, state, and Open.
A missing summary MUST display its identifier until metadata arrives.
Child links MUST remain keyed by parent thread.
History reconstruction MUST NOT apply another parent's links.
Opening a child MUST preserve a route back to its parent.
Parent-approved child requests MUST retain the child's request identifier and thread identity.
Watching both parent and child MUST NOT duplicate requests or transcript updates.
The behavior MUST follow [delegation graph](delegation-graph.md) and [native protocol](native-protocol.md).

## C44: Thread management

The thread menu MUST offer Rename, Archive, and the corresponding archived-thread recovery view.
Rename MUST show the current title, validate the new title, and call `thread/name/set`.
Success MUST update the header, navigation, and window title from authoritative state.
Failure MUST retain the user's proposed title.

Archive MUST identify the target thread before calling `thread/archive`.
It MUST explain that archiving retains the daemon journal while changing thread visibility.
For a remote runner, it MUST explain that archiving the last bound thread destroys its box.
The confirmation MUST identify that workspace risk and offer Pull before archive.
Running threads MUST require interruption to finish before archive proceeds.
Successful archive MUST remove the thread from the active list.
An archived view MUST request `thread/list` with `archived: true`, its limit, and its cursor.
This archive-only filter extends the active-navigation request rule in C02.
That view MUST support filtering and Unarchive through `thread/unarchive`.
Unarchive MUST restore the thread without duplicating it in navigation.
Unarchive MUST NOT imply that a destroyed remote box or its unpulled files were restored.
The GUI MUST NOT offer journal deletion as an archive action.

## C45: Endpoints and remotes

The remote panel MUST show configured daemon endpoints and the current endpoint.
It MUST distinguish a remote daemon from a remote execution runner.
Add and edit MUST validate the endpoint fields through the shared lifecycle package.
Credentials MUST remain with the shared lifecycle or backend credential owner defined in [native architecture](native-architecture.md).
The GUI MUST NOT create a second persistent credential store.
The panel MUST NOT derive remote credentials from transcript or workspace content.

Connect MUST identify the endpoint and show progress, success, or a specific failure.
Switching endpoints MUST isolate drafts, histories, requests, and credentials by endpoint identity.
It MUST release old subscriptions without stopping the daemon's running turns.
Disconnect MUST remove actionable requests and preserve recoverable local drafts.
Reconnect MUST use C01 and C07.

Endpoint listing and saving MUST use shared lifecycle `Endpoints` and `SaveRemote` operations.
Cloud management MUST use shared lifecycle `CloudUp`, `CloudPull`, and `CloudDown` operations.
The panel MUST show the resolved target and requested effect before an explicit mutation.
Long operations MUST expose progress, cancellation, and authoritative completion.
Cloud creation MUST leave credential copying off until the user explicitly selects it.
Without copying, the created endpoint MUST offer native provider login through C26.
Cloud removal MUST require confirmation naming its service and box.
That confirmation MUST state that removal destroys the remote daemon's journal and every thread in that box.
Cloudflare MUST retain the full-daemon prohibition from [runner modes](runners.md#mode-b-the-daemon-in-a-box).
Closing a panel MUST NOT claim that a completed remote mutation was cancelled.
These operations MUST follow [native architecture](native-architecture.md) rather than import terminal widgets.

## C46: Runners

The runner panel MUST display the current thread's effective mode, endpoint, workspace root, and archive mapping.
It MUST obtain runner inventory and bindings through `dalgona/remote/status`.
It MUST distinguish local execution, remote archive execution, and a remote daemon.
Configuration changes MUST state whether they affect future threads or the selected active thread.
The [runner contract](runners.md) remains authoritative for mode behavior.

Pull MUST use `dalgona/remote/pull` and show its target, progress, changed paths, and failures.
The client MUST use structured results negotiated through capabilities.
It MUST NOT parse human terminal output as an RPC schema.
Pull MUST explain that applying the diff changes the destination workspace.
Three-way conflicts MUST retain and list the resulting conflicted paths.
The result MUST NOT promise automatic rollback or a clean workspace after a conflict.
Garbage collection MUST use `dalgona/remote/gc`.
Its confirmation MUST identify the runner and removal scope.
Success MUST report the authoritative result rather than assume every candidate was removed.
File and terminal actions MUST use the selected runner's workspace mapping.

## C47: MCP servers

The MCP panel MUST show configured servers, transport, connection state, tools, and authentication state.
It MUST distinguish user configuration from workspace configuration.
Project and discovered configuration MUST remain read-only in this panel.
Untrusted workspace configuration MUST NOT execute a server on inspection.
The panel MUST expose the trust decision required by [MCP](mcp.md).
Configuration inventory and runtime status MUST match by exact server identity and source.
The panel MUST send the displayed inventory's `configVersion` to the negotiated runtime-status endpoint.
It MUST match only effective entries and require the runtime row's explicit source.
Missing provenance MUST report incompatibility. Changed inventory versions MUST reload without combining old status pages with new configuration.
Shadowed configuration MUST NOT display another entry's connected or authenticated state.
An absent runtime row MUST say not started rather than connected or healthy.
Starting, connected, failed, and authentication-required states MUST remain distinct.

User configuration reads and writes MUST use `dalgona/mcp/config/read` and `dalgona/mcp/config/write`.
Writes MUST include the optimistic revision from the loaded configuration.
A revision conflict MUST preserve edits and require reloading or reconciling the displayed change.
The editor MUST validate stdio and HTTP fields according to the MCP contract.
Secret values MUST remain masked and follow daemon-side secret storage.
Unchanged redacted values MUST use the protocol's preservation marker.
The GUI MUST NOT submit its masking placeholder as a replacement secret.
Save results MUST identify servers requiring restart or authentication.

Authentication MUST use `dalgona/auth` with the exact MCP target.
The panel MUST support browser, device, or manual methods advertised by that target.
It MUST expose cancellation, retry, status, and logout.
Server tool discovery MUST remain scoped to the selected thread's effective registry.
This panel MUST retain the existing tools-only MCP scope.
Refresh MUST report a specific discovery failure without claiming an empty registry.
No required authentication path MAY be replaced by a terminal-only instruction.

## C48: Workspace files

The files panel MUST provide runner-aware directory navigation and a compact file viewer or editor.
It MUST open the selected thread's workspace through `dalgona/workspace/open`.
Listing, metadata, reads, writes, and mutations MUST use the workspace methods in [native protocol](native-protocol.md).
Remote paths MUST remain remote and MUST NOT be resolved through desktop filesystem APIs.
Directory rows MUST show type, name, and useful size or modification details.
The panel MUST support parent navigation, path entry, refresh, and filename filtering.

Text files MUST support bounded loading, selection, search, editing, Save, and Save as.
Images MUST use the native image viewer.
Binary files MUST show metadata and offer explicit export without guessed text decoding.
Encoding failures MUST preserve the original bytes and explain why editing is unavailable.
Saving MUST use the loaded revision and detect concurrent changes.
A conflict MUST show the changed versions and require an explicit resolution.

Create file, create directory, rename, and remove MUST use their named workspace methods.
Remove MUST identify its path and recursive scope before confirmation.
Watch updates MUST refresh visible metadata without replacing an unsaved buffer.
An unsaved file MUST require Save, Discard, or Cancel before closing or changing its target.
Closing or changing the view MUST release its local workspace interest after that decision and completion of its owned operations.
The shared owner MUST call `dalgona/workspace/close` only after the final interest and terminal attachment are released.
Another window, completion request, running command, or terminal MUST retain its own interest.
An in-flight mutation MUST be reconciled before the handle is released. A `busy` response MUST preserve the handle for that cleanup.
Read-only, denied, missing, and disconnected states MUST remain distinct.
Copy path MUST preserve the workspace's path syntax.
Export MUST use a native destination chooser and the bounded transfer contract.

## C49: Interactive terminal

The terminal panel MUST provide a real terminal attached to the selected thread's runner.
It MUST use `dalgona/terminal/open`, `attach`, `read`, `write`, `resize`, `close`, and `detach`.
The panel MUST show the target shell, workspace root, and sandbox state before launch.
The protocol MUST carry the authoritative terminal identifier and stream position.
The desktop MUST NOT substitute `command/exec` for interactive operation.
The renderer and terminal emulator MUST follow [native documents](native-documents.md).

Terminal input MUST own ordinary keys while its viewport has focus.
It MUST support text selection, copy, paste, scrollback, resize, and reported exit state.
Composer interrupt shortcuts MUST NOT intercept terminal escape sequences.
Paste MUST preserve bracketed-paste behavior when the terminal requests it.
Terminal control sequences MUST NOT silently authorize clipboard writes or arbitrary external URL activation.
The platform contract MUST define explicit clipboard handling.

Explicitly closing the panel MUST release its attachment through `dalgona/terminal/detach` without claiming the process exited.
Hiding a retained panel MUST keep its attachment and lease reads.
The application MUST retain and renew its owned attachment while that terminal remains open.
Stop terminal MUST explicitly close the identified terminal process through the server.
Stop MUST keep the reader until both final cursors are collected, the user releases it, or output retention expires.
Expiry MUST release local attachment and workspace interests while preserving collected output and identifying any missing tail.
The UI MUST report a drain timeout as truncated output and display expired output as unavailable.
The fixed retention deadline MUST NOT be extended by polling, repeated Stop, or reattachment.
The terminal MUST detach before releasing its workspace reference. Another window's or terminal's workspace interest MUST remain valid.
Reopening MUST use `dalgona/terminal/list` with a fresh workspace handle.
It MUST offer attachment to still-running terminals within that authorized workspace.
Reconnect MUST resume from the protocol's stream position or report an output gap.
Each reader MUST apply ordered resize records at their output-byte positions and retain both cursors from reads and checkpoints.
An existing reader recovering from `replay_gap` MUST use `dalgona/terminal/checkpoint/open` without consuming another attachment slot.
A resize acknowledgment MUST NOT change the replicated grid before unread output reaches its resize record.
It MUST NOT duplicate input after an uncertain write.
An uncertain terminal input state MUST disable further input while retaining output, copy, and Stop.
Starting a replacement terminal MUST require an explicit action and MUST NOT replay the uncertain input.
The panel MUST show startup failure, disconnection, exit code, and retained output distinctly.

## C50: Diagnostics

The diagnostics panel MUST offer local checks and explicit online checks.
Disconnected local checks MUST use shared lifecycle `Diagnose`.
Connected checks MUST use `dalgona/diagnostics/run` with the chosen `online` value.
The panel MUST explain which checks contact providers or remote services before online execution.
Opening the panel MUST NOT start those checks automatically.

Results MUST retain the ordered doctor rows, states, details, and remediation text.
The [doctor contract](doctor.md) defines their meaning.
The panel MUST support rerun, copy, and export of masked diagnostics.
It MUST identify partial completion and cancellation.
Diagnostics MUST NOT start a daemon, repair configuration, or change credentials as a side effect.
A recommended repair MUST remain a separate explicit operation with its own result.

## C51: Failures and reporting

The failures panel MUST list and read masked records through `dalgona/failures/list` and `dalgona/failures/read`.
It MUST show the record time, kind, status, and available excerpt.
Viewing or exporting a record MUST NOT enable publication.
The record viewer MUST preserve masking and identify unavailable or removed content.

Reporting state MUST load through `dalgona/reporting/read`.
The panel MUST display the daemon's exact consent text, method, restrictions, and reporter availability.
Enabling MUST require the user to type `yes` beside that text.
The request MUST include its text hash, method, confirmation, and operation identifier.
`dalgona/reporting/enable` MUST store the consent receipt before reporting becomes enabled.
The client MUST NOT replace that operation with a settings write.

Changing the reporting method MUST invalidate the earlier consent receipt.
Reporter App authentication MUST use the generalized auth target defined in [native protocol](native-protocol.md).
It MUST provide cancellation, retry, and current account state.
App and `gh` restrictions from [reporting](report.md) MUST remain enforced.
The UI MUST NOT silently fall back to another reporter or authentication method.

Disable MUST call `dalgona/reporting/disable` and show its authoritative result.
It MUST explain removal of consent, App credentials, and queued excerpts under the reporting contract.
It MUST state that already published GitHub issues remain published.
Failures MUST remain visible and MUST NOT be presented as successful disabling.
This native consent flow explicitly replaces the existing terminal-only enablement gate for this client.
No unrelated setting, login, diagnostic, or startup action MAY enable reporting.

## Completion

All C01 through C51 requirements MUST pass [native verification](native-verification.md).
The 43-source inventory MUST remain complete when implementation begins.
An implementation change to a source behavior MUST update this mapping and its verification case.
Documentation approval alone MUST NOT be presented as runtime feature completion.
