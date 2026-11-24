# Input text

## I01. Status and authority

This contract defines required input behavior for the Gio GUI, TUI, web client, and daemon.
It is specification work, not evidence that those implementations already conform.
The inspected source baseline is `2c036c60b379e7718255a23ebd2c5079d76ec50c`.

The contract covers prompts, question answers, titles, search, review instructions, paths, credentials, completions, pasted text, and recovered drafts.
Each field retains its declared grammar and authority.
File bytes, PTY bytes, provider output, and historical journal records retain their existing contracts.
The input contract MUST NOT reinterpret them as editable user text.

[Protocol](protocol.md), [questions](ask.md), [TUI](tui.md), [web](../../dalgona/spec/web.md), and [native input](../../dalgona/spec/native-platform.md) inherit this contract.
Existing [secret masking](secrets.md) remains authoritative before model input and journal storage.
Preserving text MUST NOT bypass masking or expose a credential.

## I02. Preserve accepted text

Canonical text is the accepted sequence of Unicode scalar values after the explicitly declared input-adapter transformation.
A scalar excludes the surrogate range U+D800 through U+DFFF.
Equivalent rendered glyphs MUST NOT imply equivalent stored text or authority.

Generic text fields MUST accept valid private-use, noncharacter, unassigned, combining, variation-selector, and joiner scalars.
This includes supplementary private-use planes and deliberately entered U+FFFD.
U+FEFF inside a string is content, not a transport byte-order mark.
An unavailable font MUST NOT make otherwise valid input invalid.

Adapters MUST NOT apply NFC, NFD, NFKC, NFKD, case folding, whitespace trimming, or invisible-character removal to canonical text.
Decomposed Hangul and precomposed Hangul MUST remain distinct sequences.
Text and emoji presentation selectors, skin-tone modifiers, emoji tags, and regional indicators MUST remain intact.
Copying, draft recovery, history recall, chip expansion, follow-up editing, and submission MUST preserve that sequence.

Display projections and derived search keys MAY differ under their declared contracts.
They MUST NOT replace canonical text, wire values, path identities, command arguments, or selected option values.
The daemon MUST NOT apply a second general cleanup pass.

## I03. Apply field policy once

|Field class|Adapter policy|Validation and authority|
|---|---|---|
|Interactive multiline prompt, answer, or instructions|Convert CRLF and lone CR to LF once at insertion.|Preserve every other accepted scalar, including final LF.|
|Interactive single-line field|Reject a transaction containing a line break.|Keep the previous value and selection. Never insert only its first line.|
|Credential or token|Preserve exact accepted text, including significant spaces.|Apply the provider's explicit grammar. Never trim or normalize a failed credential.|
|Path, branch, identifier, enum, or command argument|Preserve the supplied scalar sequence.|Validate the declared grammar before accessing its resource or performing its action.|
|Direct daemon text parameter|Preserve decoded accepted text.|Do not repeat interactive newline conversion. Enforce method and framing limits.|
|Workspace file buffer or raw attachment|Preserve the source bytes and line endings.|Use its file or attachment contract. Do not apply prompt newline conversion.|
|Headless stdin adapter|Apply the explicitly documented CLI transformation.|[Protocol](protocol.md#clients) owns its trailing CR/LF removal exception.|

For single-line fields, line breaks are LF, CR, VT, FF, NEL, U+2028, and U+2029.
Interactive multiline conversion changes only CRLF and CR. Other separators retain their source scalars.
Model-authored question displays MAY project multiple lines under [Ask tool](ask.md), while retaining exact offered values.
Selecting an offered option MUST bypass draft newline conversion and send its original value unchanged.

NUL MUST fail validation for covered input fields, including model-authored question arguments and offered option values, with `disallowed_control`.
Other valid control and formatting scalars MUST remain data unless the field's explicit grammar rejects them.
Their presentation follows I08.
Validation MUST reject the complete edit or request. It MUST NOT repair a value silently.

Eligibility checks MUST remain separate from the value.
A field MAY reject a whitespace-only prompt or search without trimming a nonblank submitted value.
Question answers follow their explicit empty-string rule. Whitespace-only answers MUST NOT silently become empty.
Controls that inspect whitespace MUST use the pinned Unicode `White_Space` property, unless their field grammar explicitly specifies ASCII whitespace.

Only literal ASCII command prefixes and delimiters MAY activate application actions.
Fullwidth slash, exclamation, or commercial-at characters MUST remain text.
The explicit `//` escape removes one ASCII slash. It MUST preserve the remaining payload.
No normalization or confusable substitution may create a keyword, path grant, approval, or command.

## I04. Separate coordinate systems

Every text range MUST identify its source revision and coordinate unit.
Bare integer offsets MUST NOT cross an adapter boundary without a checked conversion.

|Unit|Required use|Forbidden substitution|
|---|---|---|
|UTF-8 bytes|Protocol byte limits, keyword spans, immutable source spans.|Rune, UTF-16, cluster, or cell counts.|
|Unicode scalars|Question quotas and APIs explicitly documented in runes.|User-perceived character or display-width counts.|
|UTF-16 code units|DOM selections and native APIs that declare UTF-16.|Byte offsets or the shared answer quota.|
|Extended grapheme boundaries|Committed caret movement, deletion, replacement, and selection.|Fixed-size rune groups or glyph indices.|
|Terminal cells|TUI wrapping, clipping, cursor placement, and hit testing.|Unicode length, font glyph count, or East Asian Width alone.|
|Shaped geometry|GUI and browser paint positions, caret rectangles, and candidate placement.|Persistent source positions.|

A conversion MUST validate source identity, range bounds, ordering, and scalar boundaries before producing an offset.
Application editing ranges MUST also meet the grapheme boundary rule in I05.
An invalid semantic range MUST fail with `invalid_offset` before any mutation.
An invalid optional highlight MUST leave the original text visible without that highlight.
Implementations MUST NOT clamp an invalid authority-bearing range to nearby content.

Keyword spans retain the exact source-byte semantics in [Keywords](keywords.md).
A valid semantic match MAY end inside a grapheme cluster.
Painting MAY expand that range to complete clusters without changing its semantic byte range or activating additional text.
Copy and editing MUST use source positions, never the expanded paint range as mutation authority.

Gio rune positions and grapheme-count operations require separate conversions under [native input](../../dalgona/spec/native-platform.md).
Browser adapters MUST validate lone surrogates before `TextEncoder` can replace them.
Transient platform composition storage MAY contain incomplete encoding. It MUST remain outside canonical text.

## I05. Edit complete grapheme clusters

Committed editing MUST use Unicode 18.0.0 default extended grapheme clusters from [UAX #29, revision 49](https://www.unicode.org/reports/tr29/tr29-49.html).
This is the selected conformance target. A runtime's advertised Unicode version alone MUST NOT prove conformance.
Implementation changes MUST pin the segmentation data, algorithm revision, and conformance artifact hashes.
The [shared corpus](input-text-cases.json) supplements the upstream [GraphemeBreakTest](https://www.unicode.org/Public/18.0.0/ucd/auxiliary/GraphemeBreakTest.txt).
Go adapters MUST segment with `internal/dal/textinput`, and web adapters MUST segment with `web/src/lib/text`.
[Text input package](../decisions/text-input.md) records the pinned data, generator, and conformance hashes.

Forward and backward movement MUST cross complete clusters in logical text order.
Backspace and Delete MUST remove a whole adjacent cluster when there is no selection.
Selections, replacement ranges, word operations, and application-directed caret placement MUST end at cluster boundaries.
Pointer hit testing MUST resolve to a valid boundary with explicit leading or trailing affinity.
Visual navigation through bidirectional runs MUST retain its mapping to those logical boundaries.

Every edit MUST resegment enough surrounding text to restore the selected algorithm's boundaries.
A fixed small lookbehind MUST NOT substitute for the required context.
Regional-indicator parity, joiners, variation selectors, combining marks, and Hangul joining MUST survive adjacent edits.
Ligatures and multiple fallback glyphs MUST NOT redefine editing boundaries.

Each accepted paste, completion, composition commit, or selected-range replacement MUST form one undoable transaction.
Undo MUST restore the exact prior text and selection. Redo MUST restore the exact accepted edit.
Neither direction may normalize or duplicate text.

A grapheme cluster has no fixed scalar or byte limit.
Field and work budgets still apply to a single unusually long cluster.
An over-limit cluster MUST be rejected whole, never split to satisfy a quota.
Platform-owned preedit edits are the exception to committed grapheme boundaries, as defined in I06.

## I06. Own the full composition lifecycle

GUI and web fields MUST distinguish committed text, composition state, selection, and an unsent rejected candidate.
The platform input adapter owns intermediate preedit text and its platform-defined ranges.
Application editing MUST NOT rewrite those ranges during composition.

Composition ownership MUST include the final commit and the key event that ends composition.
Enter used to commit or choose a candidate MUST NOT also send, steer, queue, accept, or advance a question.
Escape used to cancel composition MUST NOT also interrupt, dismiss, or close a panel.
Arrow, Tab, history, palette, and request shortcuts MUST defer while the input method owns them.
Event-order differences MUST NOT be solved by an arbitrary delay or one legacy key-code check.

The adapter MUST validate the complete committed candidate before replacing canonical text.
Composition cancellation MUST restore the previous committed value and selection.
A rejected commit MUST retain a recoverable candidate and leave the previous canonical value intact.
Focus changes MUST finish or cancel composition through the platform adapter before application actions continue.
No stale commit may enter another field, thread, endpoint, or window.

The terminal emulator owns TUI preedit and its candidate window.
The TUI MUST NOT infer unreported composition state from timing or invent platform events.
Committed rune and paste events MUST remain text. They MUST NOT be replayed as individual shortcut keys.
Terminal key protocols and the declared emulator/IME pairs require real integration tests under I12.

## I07. Make insertion and quotas atomic

All fields MUST validate an entire insertion before changing their committed value.
This covers typing batches, paste, drag text, accessibility edits, IME commits, completion, history, and restored drafts.
Async results MUST bind endpoint, window, thread, field, draft revision, and selection before insertion.
A changed binding MUST discard the result without modifying the current draft.

Question answers MUST contain at most 2000 Unicode scalars, as defined in [Ask tool](ask.md).
This permits at most 8000 decoded UTF-8 bytes, independently of encoded JSON size.
An emoji outside the BMP counts as one scalar and two UTF-16 code units.
A joined emoji can contain several scalars while remaining one editing cluster.
HTML `maxlength`, Go byte length, and cluster count MUST NOT implement this quota.

Other fields MUST use their declared method, attachment, and request budgets.
The first-party composer input-array budget remains 15 MiB of encoded JSON under the client contracts.
The complete message MUST also fit the connection's framing limit after metadata and JSON escaping.
A valid field count MUST NOT bypass a smaller enclosing budget.
Count complete encoded UTF-8 bytes. Do not estimate them from JavaScript string length or glyph count.

Rejection MUST leave the previous canonical text, selection, attachments, and pending request identity intact.
The field MUST show an actionable error without sending a truncated prefix.
An over-limit candidate MUST remain recoverable within bounded intake storage, or through its unchanged original source.
Recovery MUST NOT require unlimited buffering or write credential contents to diagnostics or ordinary draft storage.
The user MUST be able to reduce or replace the candidate deliberately.

Paste-chip identities MUST use tagged metadata outside the Unicode text namespace.
Private-use scalars MUST NOT become hidden attachment markers or reference unrelated chips.
Expanding or removing a chip MUST act on its identity, not a matching user character.

## I08. Render safely without rewriting data

Display handling MUST preserve a mapping from visible positions to canonical source positions.
No renderer may normalize text after computing selection, widths, or hit-test geometry.
Font fallback, shaping, and display escaping MUST share that mapping.

Terminals MUST NOT execute escape sequences from user, model, server, or pasted text outside a declared raw-terminal surface.
Accepted non-layout controls MUST have visible escapes in editable fields and decision details.
A visible escape MUST read `<U+`, four to six uppercase hexadecimal digits, and `>`.
An exposed projection escapes every scalar except printable ASCII, and it escapes `<` so distinct sources keep distinct projections.
LF and TAB MAY use their declared line and tab layout.
Model-authored question projections retain the explicit display transformations in [Ask tool](ask.md).
Bidirectional controls MUST be inspectable and isolated from surrounding labels, paths, endpoints, and action names.
Natural right-to-left text MUST remain supported. Non-ASCII text alone MUST NOT trigger a warning.
An escaped display MUST copy the original accepted source through its normal copy action.
A separate copy-as-escaped action MUST identify its transformation.

GUI and web text MUST enter text rendering APIs, never executable HTML, script, or command interpolation.
An invisible formatting character MUST NOT change which resource an action addresses.
Distinct offered options MUST remain distinguishable even when their ordinary glyphs appear identical.
A code-point inspection or escaped display MUST expose any hidden difference before selection.

Missing glyphs MUST remain selectable and preserve their original scalars through copy and submission.
Fallback MAY use a visible missing-glyph box with an inspectable code-point label.
That box or label MUST NOT replace the source with U+FFFD or literal escape text.
Color emoji support and font coverage MUST be verified independently of text preservation.

## I09. Keep terminal geometry consistent

The TUI MUST use one declared width profile for wrapping, clipping, caret placement, selection, and hit testing.
The profile MUST name its segmenter, terminal assumptions, ambiguous-width setting, emoji-width policy, and tab rule.
The existing `RUNEWIDTH_EASTASIAN` setting and tab stops remain governed by [TUI](tui.md#width).
[East Asian Width](https://www.unicode.org/reports/tr11/) alone MUST NOT serve as the terminal width algorithm.

Clipping MUST keep complete editing clusters and complete visible control escapes.
Tabs, wide cells, combining sequences, missing glyphs, and wrapped continuation rows MUST retain source mappings.
A terminal width mismatch MUST NOT corrupt the draft or move a mutation to another scalar.
Supported emulator profiles MUST have measured cursor-position and wrap fixtures.
The project MUST NOT claim identical cell geometry across every terminal emulator or font.

## I10. Validate before daemon effects

The daemon MUST reject malformed UTF-8 and unpaired JSON surrogate escapes before dispatch.
It MUST distinguish malformed encoding from an intentional U+FFFD scalar.
Transport decoders MUST retain incomplete UTF-8 bytes across reads until the frame is complete.
An incomplete sequence at frame completion or EOF MUST fail, never become accepted replacement text.

Every known text field in a mutating request MUST pass its complete policy before any request effect.
One invalid text item MUST reject the whole request before tool discovery, trust changes, file reads, credential updates, or journal appends.
Error codes, ASCII reasons, and field paths follow [Protocol](protocol.md#text-inputs).
Errors MUST NOT echo a credential or untrusted control-bearing value.

Accepted values MUST preserve their scalar sequence through serialization, daemon conversion, replay, and client reconstruction.
JSON escape spelling MAY differ without changing decoded text.
The existing turn item assembly and secret masking remain the named transformations.
Validation MUST occur before those transformations can write blobs or journal events.
No migration may normalize historical records or change journal bytes, sequence, CRC, or hash-chain inputs.

Text chunks MAY split an extended grapheme cluster while retaining their declared UTF-8 framing rules.
Consumers MUST reassemble source text and resegment its growing tail before editing or final layout.
File-page and raw-stream boundaries MAY split UTF-8 bytes under their separate incremental decoder contracts.
Chunk boundaries MUST NOT insert replacement characters, extra grapheme breaks, or duplicate text.

## I11. Keep rejected answers correctable

Text validation MUST happen before an otherwise structurally valid answer claims the question's first-valid-answer decision.
An invalid answer MUST leave its waiter, request ID, draft, subscriber set, and original deadline pending.
The existing `warning` notification carries the correction field and reason under [Protocol](protocol.md#server-requests).
The host MUST NOT send a JSON-RPC error response to a response.

First-party clients MUST keep the card and typed draft until `serverRequest/resolved` arrives.
Their typed response lifecycle MUST permit a corrected response using the same request ID after a correction warning.
Preserving only the server waiter cannot satisfy this requirement.
An invalid answer MUST NOT resolve as answered, skipped, cancelled, or declined.
A stale warning MUST NOT reopen a resolved card or reset its timeout.

Wrong answer shapes, client error responses, disconnects, and last-subscriber loss retain their existing outcomes.
Legacy clients MAY lack correction UI. Reconnecting MUST NOT be presented as a guarantee that a pending question survives.
Exactly one valid answer or existing terminal outcome may win.

## I12. Prove cross-client behavior

The [fixture corpus](input-text-cases.json) records literal scalar sequences and independent expected counts and grapheme boundaries.
Offsets in `graphemeBoundaries` are scalar indices, including zero and the exclusive end.
`text` uses JSON escapes so invisible and supplementary scalars remain reviewable.
`policy` describes generic input acceptance before any named field-specific grammar.
The raw CRLF vector measures source text before the interactive newline adapter.

Each GUI, TUI, web, and daemon implementation MUST consume the same valid and malformed fixtures.
Daemon tests assert preservation and validation, not cursor geometry or composition.
Client tests MUST exercise actual editor and transport adapters, not a duplicate test-only implementation.
The upstream segmentation corpus MUST supplement these product-specific vectors.

|Case|Required result|
|---|---|
|Type, paste, select, copy, undo, redo, recall, submit, and replay each valid vector|Exact accepted scalar sequence remains unchanged at every applicable boundary.|
|Delete around Hangul, supplementary CJK, combining marks, IVS, and joined emoji|Every committed edit uses the declared grapheme boundary.|
|Insert or delete a regional indicator, joiner, or combining mark|Resegmentation repairs adjacent boundaries and caret affinity.|
|Compare composed and decomposed text, or text and emoji selectors|Distinct sequences stay distinct after every round trip.|
|Paste private-use text beside a paste chip|No character becomes a chip reference or disappears.|
|Submit 2000 and 2001 scalar answers|The first succeeds. The second remains correctable without partial acceptance.|
|Submit 2000 supplementary emoji scalars|4000 UTF-16 units still satisfy the scalar quota when all other limits pass.|
|Insert one cluster containing 2001 scalars into an answer|The complete edit fails. No prefix or combining suffix is accepted.|
|Insert one cluster containing 100001 scalars|Bounded validation rejects an answer without pathological segmentation work or partial insertion.|
|Paste multiline text into a single-line field|The whole edit fails and the previous value and selection remain.|
|Paste CRLF into a multiline draft, then save and restore|Conversion occurs once. The final LF remains present.|
|Split every multibyte scalar across transport reads|Reassembly produces the original text exactly once.|
|Send malformed UTF-8, surrogate escapes, or a browser lone surrogate|The relevant boundary rejects before replacement encoding or effects.|
|Place one invalid text item among valid request items|The complete request fails with no partial effect or journal append.|
|Send a keyword span inside a scalar or from another revision|The original text remains visible without the invalid highlight.|
|Use a valid keyword span ending before a combining mark|Only its paint range expands. Semantic activation stays unchanged.|
|Use controls, bidirectional formatting, and missing glyphs in a field|Display stays safe, differences remain inspectable, and source copies intact.|
|Return an async completion after focus or revision changes|No stale text enters the current field.|
|Reject one answer, then correct it from the same client|The same request resolves once with its original deadline.|
|Race correction, another client's answer, timeout, and disconnect|One existing terminal outcome wins. No stale warning revives the question.|
|Exceed encoded JSON size with otherwise valid text|The full draft remains unsent with an explicit size error.|

Real IME evidence MUST cover Korean Hangul, Japanese conversion, and Chinese Pinyin on each declared GUI and browser platform.
The matrix MUST name OS, application version, input method, browser or terminal, display backend, and font configuration.
It MUST exercise candidate navigation, selection replacement, commit, cancellation, focus changes, and over-limit commits.
Synthetic composition events and rendered CJK strings MUST NOT substitute for actual input-method evidence.
TUI evidence MUST name supported terminal/IME pairs and test bracketed paste alongside committed text delivery.
Headless passing tests MUST NOT be reported as visual, terminal-width, or IME acceptance.

## I13. Required implementation changes

The source baseline contains the following migration work.
These observations identify required changes, not a claim that this documentation changes runtime behavior.

|Source|Observed behavior|Required change|
|---|---|---|
|[TUI input](../../../internal/dal/tui/input.go)|Single-line paste keeps its first line. Private-use scalars encode chips.|Reject multiline edits whole and keep chip metadata outside text.|
|[TUI sanitization](../../../internal/dal/tui/sanitize.go)|Cleanup removes formatting, noncharacters, and supplementary private-use text.|Separate canonical input from safe display projection.|
|[Web answers](../../../web/src/lib/question.ts), [question card](../../../web/src/lib/components/QuestionCard.svelte)|Answer processing trims and truncates. HTML length uses UTF-16.|Use the shared scalar quota and atomic rejection.|
|[Web composer](../../../web/src/lib/components/Composer.svelte)|Submission trims typed text.|Separate send eligibility from preserved payload.|
|[Answer cleanup](../../../internal/dal/tool/ask/clean.go), [results](../../../internal/dal/tool/ask/result.go)|User answers are cleaned and truncated.|Validate exact text before claiming a question outcome.|
|[RPC connection](../../../internal/dal/rpc/conn.go)|The response dispatcher consumes its waiter before typed answer validation.|Retain pending questions through correction and migrate first-party response ownership.|
|[Agent submission](../../../internal/dal/agent/entry.go)|Tool preparation precedes complete shared text validation.|Validate before discovery, masking storage, and journal effects.|
|[CLI input](../../../internal/dal/app/cli.go)|Invalid stdin UTF-8 receives replacement characters.|Reject malformed input and retain the explicitly named newline exception.|

An implementation MUST migrate every affected caller together for each changed boundary.
Tests MUST detect the listed defects through behavior and independent expected text.
Source assertions, snapshot updates, and a passing style check MUST NOT count as Unicode behavior proof.

## I14. Standards and API references

The following primary sources explain the units and boundaries used by this contract.
Product rules above choose the field limits and supported behaviors.

|Source|Use|
|---|---|
|[UAX #29 revision 49](https://www.unicode.org/reports/tr29/tr29-49.html)|Selected default extended grapheme segmentation.|
|[Unicode normalization](https://www.unicode.org/reports/tr15/)|Canonical and compatibility normalization are explicit transformations.|
|[Unicode private-use and noncharacter FAQ](https://www.unicode.org/faq/private_use.html)|Valid scalar categories do not imply assigned public glyphs.|
|[Emoji specification](https://www.unicode.org/reports/tr51/tr51-31.html)|Presentation selectors, modifiers, flags, tags, and joined emoji.|
|[Bidirectional algorithm](https://www.unicode.org/reports/tr9/)|Logical source order and visual display order.|
|[East Asian Width](https://www.unicode.org/reports/tr11/)|Character properties and their limits for terminal layout.|
|[Gio widget API](https://pkg.go.dev/gioui.org/widget)|Rune offsets, grapheme-count editing, and editor ownership.|
|[Gio input architecture](https://gioui.org/doc/architecture/input)|Input routing and frame ownership.|
|[HTML text selection](https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#textFieldSelection)|Browser text selection uses UTF-16 code units.|
|[Encoding API](https://encoding.spec.whatwg.org/#interface-textencoder)|Boundary validation before replacement-capable string conversion.|
|[Input Events](https://www.w3.org/TR/input-events-2/), [composition events](https://www.w3.org/TR/uievents/#events-compositionevents)|Preedit, commit, event ownership, and noncancelable input cases.|
