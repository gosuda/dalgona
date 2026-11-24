# Ask tool

The `ask` tool lets the working model put questions to the user and wait for the answers.

## Tool

The tool name MUST be `ask`.

The tool MUST be a read-class tool.

The tool MUST NOT journal an event of its own. The question is the `tool.started` arguments, and the answer is the `tool.completed` output.

The description of the tool MUST be the text below.

```text
Ask the user 1 to 4 questions and wait for the answers. Use it only when the choice belongs to the user and the code, the docs and the conversation cannot settle it: a preference, a trade-off, or a missing requirement. Do not use it to ask permission to run a tool or to report progress.

Each question has a `header` (a tab label of at most 30 characters) and a `question` (one sentence). Give 2 to 6 `options`, each with a `label` of a few words and a `description` of what that choice means. Put the option you recommend first; the user sees it marked as recommended. Add `preview` (plain text, at most 40 lines, shown in monospace) when the user must compare concrete output such as code or a layout. Set `multi_select` to true when several options can apply. Omit `options` for a free-text question.

Every question with options gets an Other row where the user can type an answer, so never add an option named Other. The result lists one line per question; a typed answer appears in quotes after `Other:`. When the user skips, dismisses, or cannot be reached, the result says so; continue with your best judgment and state the assumptions you made.

If the user does not answer within 5 minutes, the result says so and uses the first option of each question that has options, so make the first option one that is safe to use without an answer.
```

## Arguments

The arguments MUST be an object with one required member, `questions`.

In this specification, `characters` means Unicode scalar values.
Cursor movement and deletion MUST use extended grapheme clusters under [Input text](input-text.md).
Argument limits MUST count accepted decoded text before any display projection.

The arguments MUST match the schema below. The schema sets `additionalProperties` to false at every level.

```json
{"type":"object","properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","properties":{"header":{"type":"string","maxLength":30,"description":"Tab label of a few words."},"question":{"type":"string","maxLength":500,"description":"The question as one sentence."},"options":{"type":"array","minItems":2,"maxItems":6,"description":"2 to 6 choices, recommended first. Omit for a free-text question.","items":{"type":"object","properties":{"label":{"type":"string","maxLength":80},"description":{"type":"string","maxLength":300},"preview":{"type":"string","maxLength":2000}},"required":["label","description"],"additionalProperties":false}},"multi_select":{"type":"boolean","description":"Let the user pick several options."}},"required":["header","question"],"additionalProperties":false}}},"required":["questions"],"additionalProperties":false}
```

The tool MUST accept from 1 to 4 questions per call.

A question MUST have a `header` of 1 to 30 characters.

A question MUST have a `question` of 1 to 500 characters.

A question MAY have `options`. A question without `options` is a free-text question.

A question with `options` MUST have from 2 to 6 options.

A question MUST NOT set `multi_select` to true unless it has `options`.

An option MUST have a `label` of 1 to 80 characters and a `description` of 1 to 300 characters.

An option MAY have a `preview` of at most 2000 characters and at most 40 lines.

Required headers, questions, option labels, and descriptions MUST also pass a nonblank eligibility check before the host offers the question.
That check MUST ignore complete escapes in the profile below, Unicode `White_Space`, `General_Category=Cc`, and `Default_Ignorable_Code_Point` scalars.
If no scalar remains, validation MUST use the existing empty-field error for that field.
The eligibility projection MUST NOT replace accepted source text or change the scalar count used for its upper limit.
Optional previews and submitted user answers retain their separate empty-value rules.

Two options of one question MUST NOT have the same label, compared without case.

No option MUST have the label `Other`, in any case. The user always gets an Other row.

Two questions of one call MUST NOT have the same text, compared without case.

An unknown member at any level MUST fail the call with `ask: unknown field "<name>"`.

The tool MUST fail the call with the messages below when a limit is broken. The number after `question` or `option` is the 1-based position.

| Condition | Message |
|---|---|
| Question count outside 1 to 4 | `ask: questions needs 1 to 4 items, got <n>` |
| Empty or ineligible blank header | `ask: question <i>: header is empty` |
| Header over 30 characters | `ask: question <i>: header is longer than 30 characters` |
| Empty or ineligible blank question | `ask: question <i>: question is empty` |
| Question over 500 characters | `ask: question <i>: question is longer than 500 characters` |
| Option count outside 2 to 6 | `ask: question <i>: options needs 2 to 6 items, or none for a free-text question, got <n>` |
| `multi_select` without options | `ask: question <i>: multi_select needs options` |
| Empty or ineligible blank label | `ask: question <i> option <j>: label is empty` |
| Label over 80 characters | `ask: question <i> option <j>: label is longer than 80 characters` |
| Empty or ineligible blank description | `ask: question <i> option <j>: description is empty` |
| Description over 300 characters | `ask: question <i> option <j>: description is longer than 300 characters` |
| Preview over 2000 characters or 40 lines | `ask: question <i> option <j>: preview is longer than 2000 characters or 40 lines` |
| NUL in a header or question | `ask: question <i>: <field> contains NUL` |
| NUL in a label, description, or preview | `ask: question <i> option <j>: <field> contains NUL` |
| Repeated label | `ask: question <i> option <j>: label "<label>" repeats option <k>` |
| Label `Other` | `ask: question <i> option <j>: the user always gets an Other row; remove this option` |
| Repeated question | `ask: question <i> repeats question <k>` |
| Arguments of the wrong shape | `ask: arguments do not match the schema: <error>` |

## Text validation and display

Every text field MUST follow [Input text](input-text.md).
Malformed UTF-8 and unpaired JSON surrogate escapes MUST fail validation instead of becoming U+FFFD.
Literal U+FFFD is valid content and MUST NOT be mistaken for a decoding failure.
NUL MUST fail validation in question arguments and answers before the host offers or accepts them.

The cleaning rules below apply only to derived displays of model-authored questions.
They MUST NOT rewrite submitted user answers or an offered option's wire value.
The host MUST retain each exact offered option label for comparison and submission.
Model-authored question fields remain display content under the shared field matrix.
Painting a question on one line MUST NOT apply the single-line user-input rejection rule to its source.
Its source MAY contain line breaks that the display projection represents as spaces.

A terminal escape sequence MUST NOT execute in a display.
A terminal projection MAY remove complete sequences recognized by the profile below, including their parameters.
Sequence recognition MUST preserve UTF-8 scalar boundaries and recognize ASCII control syntax before consuming a span.
An incomplete or invalid escape prefix MUST display its ESC as a visible code-point escape without consuming following ordinary scalars.
The projection MUST then process those scalars under their usual display rules.
A non-ASCII scalar after ESC is not an ASCII escape final byte and MUST remain intact.
UTF-8 continuation bytes MUST NOT be interpreted as standalone C1 controls.
Required-field eligibility and display projection MUST use the same complete-sequence recognition rule.
Other control and bidirectional formatting characters MUST use the safe display policy in [Input text](input-text.md).

In a single-line question projection, a newline MUST become one space, and a tab MUST become one space.

In a preview projection, a newline MUST stay, and a tab MUST become four spaces.

A single-line projection MAY omit leading and trailing spaces.
A preview projection MAY omit leading blank lines and trailing spaces.
Those display choices MUST NOT modify the accepted source text.
An accepted required field MUST remain visible after projection, using source-preserving fallback when its font lacks a glyph.

Two distinct offered labels MUST remain distinguishable after projection.
When a projection hides their difference, the display MUST expose the differing code points with visible escapes.
Selecting either row MUST submit its exact offered label, never its display projection.

Counting limits before projection and preserving offered values tighten the previous cleaning behavior.
The host keeps each accepted source. Results and terminal displays project it with `DisplayLine` and `DisplayBlock` from `internal/dal/textinput`.

### Escape recognition profile

Question eligibility and display MUST share the following recognition profile.
It selects seven-bit forms for safe question display. It is not the raw PTY parser or full XTerm command support.
Unsupported forms MUST use visible escaping and preserve their ordinary text.
Only ASCII ESC starts a candidate. Recognizers MUST NOT mask high bits or interpret UTF-8 continuation bytes as controls.

A header contains zero or more ASCII parameter bytes, then zero or more intermediate bytes, then one final byte.
Parameter bytes are `0x30` through `0x3f`. Intermediate bytes are `0x20` through `0x2f`.
The mandatory final byte is `0x40` through `0x7e`.
ST means ESC followed by one U+005C.

|Form|Complete recognition rule|
|---|---|
|CSI|ESC, `[`, and one header.|
|DCS|ESC, `P`, one header, a payload, and ST. The header MUST complete before any payload is scanned.|
|OSC|ESC, `]`, one or more ASCII decimal digits, then BEL or ST. A semicolon MAY introduce a payload before that terminator.|
|SOS, PM, APC|ESC, respectively `X`, `^`, or `_`, a payload, and ST.|
|Other short escape|ESC, zero or more intermediate bytes, and one ASCII final byte `0x30` through `0x7e`.|

The listed introducers MUST use their own full rule. They MUST NOT fall back to the short-escape rule.
A payload contains zero or more complete UTF-8 scalars, subject to the terminator and invalidation rules below.
OSC payloads are opaque after the decimal selector and semicolon. DCS payloads are opaque only after a valid header.
OSC forms with a missing decimal selector or a legacy letter selector MUST use visible escaping.
This includes unsupported forms even when a terminal might recognize them.
Every payload MUST be scanned as complete UTF-8 scalars within the field limit.

CAN, SUB, or an ESC that does not begin the required ST MUST invalidate the pending candidate.
Recognition MUST NOT cross that boundary to borrow a terminator from a later sequence.
The display MUST visibly escape the original ESC, then rescan its suffix under the ordinary rules without dropping text.
Eligibility MUST use that same recognition result before ignoring its separately specified whitespace, control, and formatting scalars.
These rules do not interpret command payloads, execute terminal actions, or change canonical text.

The profile uses control forms from [XTerm](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) and the [DEC VT510 manual](https://www.zx.net.nz/documentation/mds-199909/cd3/term/vt510rmb.pdf).
The DCS header is the selected DEC form, not a requirement for every device-defined control string in ECMA-48.

## Results

The tool result MUST start with the line `User answers:` when the user answered at least one question.

An answered question MUST appear as one line in the form `- <header> (<question>): <answers>`.

Answers MUST be joined with `; `.

An answer that equals an option label MUST appear as the label.

An answer that is typed text MUST appear in double quotes. On a question with options, the quoted text MUST follow `Other: `.

Quoted answers MUST escape line breaks and control characters without changing their decoded scalar sequence.
The quote MUST decode as a JSON string to the exact answer. It escapes controls, line breaks, and directional controls as `\uXXXX`.
Option labels with unsafe display characters MUST use their safe escaped projection in the result.
The host MUST match the original offered label before projecting it.

A question with no answer MUST show `no answer`.

A question MUST NOT show more answers than one, unless it is a multi-select question. A multi-select question MUST NOT show more answers than its options plus one.

An answer MUST NOT exceed 2000 Unicode scalar values, which bounds its decoded UTF-8 size to 8000 bytes.
This limit is independent of encoded JSON frame limits and grapheme counts.
An over-limit answer MUST fail validation whole.
The host MUST NOT truncate it, split a grapheme cluster, clean it, or silently discard a suffix.
Interactive clients MUST apply the shared newline policy once to newly entered free-form answer text before checking and submitting it.
Selecting an offered option MUST preserve its exact value, including line endings, without passing through the free-form input adapter.
The daemon MUST preserve the accepted answer, including leading and trailing whitespace.
Empty strings retain the existing no-answer meaning.
Whitespace-only strings MUST NOT become empty through cleanup.

Option matching and answer deduplication MUST use exact decoded scalar sequences.
They MUST NOT use display projections, normalized search keys, or case folding.

When every question has no answer, the result MUST be the skipped text below.

A cancelled question MUST return plain text. It MUST NOT return an error.

The recommended option of a question MUST be its first option. A timed-out result MUST use it for each question that has options.

The fixed result texts MUST be these.

| Case | Text | Success |
|---|---|---|
| Every question skipped | The user skipped the questions and has no preference. Choose for yourself and say which choice you made. | yes |
| Dismissed | The user dismissed the questions without answering. Continue with your best judgment, or stop and say what you need if you cannot continue. | yes |
| No client | declined: no client could answer this question | no |
| Interrupted | interrupted: the user interrupted the turn before answering | no |
| Host restarted | interrupted: the host restarted before the user answered | no |
| Timed out with options | No answer before the timeout. The recommended options were used instead of the user's answers: | yes |
| Timed out without options | No answer before the timeout. The user has not chosen. Choose for yourself and say which choice you made. | yes |
| Other failure | declined: followed by the failure detail | no |

## Host path

The host MUST offer each question to every subscribed client.

A question with no subscribed client MUST end at once with the no-client result.

A subscriber that joins MUST receive every open question, oldest first.

When the last subscriber leaves, the host MUST end every open question with the no-client result.

A window that cannot answer MUST count once for each question. The host MUST stop counting a window when it leaves.

A subscriber that the host closes as slow MUST NOT end the open questions, because the server subscribes again and receives them.

The first valid answer to a question MUST decide it. A later answer MUST fail with an unknown request error.

The host MUST validate the entire answer text before claiming that decision.
The correction rules below apply to text-validation failures in an otherwise structurally valid answer result.
A result with the wrong answer shape retains the existing unreadable-answer failure outcome.
A text-validation failure MUST NOT become a declined, cancelled, skipped, or answered outcome.
It MUST NOT change the question's request id, original deadline, or other subscribers.
The transport MUST keep the response waiter pending and send the correction warning defined in [Protocol](protocol.md#server-requests).
The user MAY correct the draft and answer the same pending request.
First-party clients MUST retain their question card until `serverRequest/resolved`, including after they send an answer.
Their typed question lifecycle MUST retain the ability to send a corrected response after that first send.
Each correction MUST use the original request id and follow a matching correction warning.
Sending a response MUST NOT release the draft, card, or response sender while the question remains pending.
The host MUST NOT reset the five-minute timeout when validation fails.

These retry rules require typed response validation before the transport removes its pending response waiter.
They also require migrating the current one-response callback path to the typed question lifecycle in [Protocol](protocol.md#server-requests).
Retaining the server waiter alone does not implement client correction.
Older clients MAY lack a correction interface after sending an invalid answer.
That text-validation failure MUST NOT decide the host question.
The existing last-subscriber, interruption, and timeout rules still apply.
Reconnecting or reopening MUST NOT be promised as recovery, because losing the last subscriber can resolve the question.

An interrupt of the turn MUST end the question as interrupted. A cancellation that races the question offer MUST also end the question as interrupted and MUST resolve its update as resolved. A question already decided MUST keep its outcome. The host MUST NOT send a request whose outcome it already sent.

The host MUST end an open question with the timed-out outcome when 5 minutes have passed since it published the question to its first subscriber.

A later subscriber MUST NOT restart the timer.

The host MUST stop the timer when it ends the question for any other cause.

The timeout MUST NOT be configurable.

A consumer other than the `ask` tool MUST read a timeout as no answer and MUST NOT use a recommended option.

The host MUST mask the header, question, label, description and preview of each question before it offers the question.

The host MUST NOT journal a question or its answer.

The thread status MUST count the open questions.

## Scope

The `ask` tool MUST be available in top-level threads of `dal` and `dalgona`.

A subagent MUST NOT get the `ask` tool, because no user reads a subagent's questions.

An advisor thread MUST NOT get the `ask` tool, because an advisor waits for no person.

A call to `ask` in a subagent MUST complete with the error `Tool ask not found`.

Recovery MUST NOT ask an open question again. It MUST complete the call as interrupted, as the recovery table of [recovery.md](recovery.md) states.

The rule MUST be enforced by `withThreadTools` in `internal/dal/agent/spawn.go`. `TestAskToolOnlyOnTopLevelThreads` and `TestAskToolInDal` in `internal/dal/agent/ask_tool_test.go` check it.

## Input acceptance cases

These cases are required implementation evidence, not results recorded by this documentation change.

|Case|Required observation|
|---|---|
|Required field containing only U+0020, U+00A0, U+3000, TAB, or LF|The host rejects it with that field's empty-value error before publishing a question.|
|Required field containing only terminal escape sequences, directional controls, U+200D, or U+FE0F|The nonblank check rejects it without creating an empty visible question or option.|
|Valid required text surrounded by spaces or formatting scalars|Its full accepted source is preserved and counted against its upper limit.|
|Valid required text whose glyph is unavailable|The field remains visible and inspectable through source-preserving fallback.|
|ESC followed by U+D55C, a supplementary scalar, a combining sequence, or a private-use scalar|ESC is visibly escaped. Following scalars remain intact, visible, and mapped to their original source.|
|An incomplete CSI or control-string prefix before valid text|The prefix cannot swallow the field or split UTF-8. No display emits an executable control.|
|DCS followed by U+D55C before an ASCII final byte, then ST|The candidate is invalid. The visible fallback preserves the ordinary scalars.|
|OSC followed by U+D55C instead of a decimal selector, then BEL|The candidate is invalid. The visible fallback preserves the ordinary scalars.|
|A valid DCS header before its payload, or OSC with a decimal selector and payload|The complete recognized span is removed only from the derived eligibility or display projection.|
|A valid DCS header or OSC selector and semicolon followed immediately by its terminator|The empty payload is valid. Eligibility and display recognize the same complete span.|
|CAN, SUB, or a non-ST ESC inside a candidate before a later ST|The first candidate is invalid. Later text and independently valid controls are scanned without borrowing that ST.|
|An offered option containing CRLF and leading spaces|Selection returns its exact offered value without draft newline conversion or trimming.|
|A whitespace-only user answer|It follows the answer contract and is not rewritten to an empty answer.|
|A 2001-scalar answer followed by a valid correction|The same pending request accepts the correction once without changing its original deadline.|
