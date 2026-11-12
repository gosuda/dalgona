# Ask tool

This topic specifies the `ask` tool, which lets the working model put questions to the user and wait for the answers.

## Tool

The tool name MUST be `ask`.

The tool MUST be a read-class tool.

The tool MUST NOT journal an event of its own. The question is the `tool.started` arguments, and the answer is the `tool.completed` output.

The description of the tool MUST be the text below.

```text
Ask the user 1 to 4 questions and wait for the answers. Use it only when the choice belongs to the user and the code, the docs and the conversation cannot settle it: a preference, a trade-off, or a missing requirement. Do not use it to ask permission to run a tool or to report progress.

Each question has a `header` (a tab label of at most 30 characters) and a `question` (one sentence). Give 2 to 6 `options`, each with a `label` of a few words and a `description` of what that choice means. Put the option you recommend first; the user sees it marked as recommended. Add `preview` (plain text, at most 40 lines, shown in monospace) when the user must compare concrete output such as code or a layout. Set `multi_select` to true when several options can apply. Omit `options` for a free-text question.

Every question with options gets an Other row where the user can type an answer, so never add an option named Other. The result lists one line per question; a typed answer appears in quotes after `Other:`. When the user skips, dismisses, or cannot be reached, the result says so; continue with your best judgment and state the assumptions you made.
```

## Arguments

The arguments MUST be an object with one required member, `questions`.

The arguments MUST match the schema below. The schema sets `additionalProperties` to false at every level.

```json
{"type":"object","properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","properties":{"header":{"type":"string","maxLength":30,"description":"Tab label of a few words."},"question":{"type":"string","maxLength":500,"description":"The question as one sentence."},"options":{"type":"array","minItems":2,"maxItems":6,"description":"2 to 6 choices, recommended first. Omit for a free-text question.","items":{"type":"object","properties":{"label":{"type":"string","maxLength":80},"description":{"type":"string","maxLength":300},"preview":{"type":"string","maxLength":2000}},"required":["label","description"],"additionalProperties":false}},"multi_select":{"type":"boolean","description":"Let the user pick several options."}},"required":["header","question"],"additionalProperties":false}}},"required":["questions"],"additionalProperties":false}
```

The tool MUST accept from 1 to 4 questions per call.

A question MUST have a `header` of 1 to 30 characters after cleaning.

A question MUST have a `question` of 1 to 500 characters after cleaning.

A question MAY have `options`. A question without `options` is a free-text question.

A question with `options` MUST have from 2 to 6 options.

A question MUST NOT set `multi_select` to true unless it has `options`.

An option MUST have a `label` of 1 to 80 characters and a `description` of 1 to 300 characters.

An option MAY have a `preview` of at most 2000 characters and at most 40 lines.

Two options of one question MUST NOT have the same label, compared without case.

No option MUST have the label `Other`, in any case. The user always gets an Other row.

Two questions of one call MUST NOT have the same text, compared without case.

An unknown member at any level MUST fail the call with `ask: unknown field "<name>"`.

The tool MUST fail the call with the messages below when a limit is broken. The number after `question` or `option` is the 1-based position.

| Condition | Message |
|---|---|
| Question count outside 1 to 4 | `ask: questions needs 1 to 4 items, got <n>` |
| Empty header | `ask: question <i>: header is empty` |
| Header over 30 characters | `ask: question <i>: header is longer than 30 characters` |
| Empty question | `ask: question <i>: question is empty` |
| Question over 500 characters | `ask: question <i>: question is longer than 500 characters` |
| Option count outside 2 to 6 | `ask: question <i>: options needs 2 to 6 items, or none for a free-text question, got <n>` |
| `multi_select` without options | `ask: question <i>: multi_select needs options` |
| Empty label | `ask: question <i> option <j>: label is empty` |
| Label over 80 characters | `ask: question <i> option <j>: label is longer than 80 characters` |
| Empty description | `ask: question <i> option <j>: description is empty` |
| Description over 300 characters | `ask: question <i> option <j>: description is longer than 300 characters` |
| Preview over 2000 characters or 40 lines | `ask: question <i> option <j>: preview is longer than 2000 characters or 40 lines` |
| Repeated label | `ask: question <i> option <j>: label "<label>" repeats option <k>` |
| Label `Other` | `ask: question <i> option <j>: the user always gets an Other row; remove this option` |
| Repeated question | `ask: question <i> repeats question <k>` |
| Arguments of the wrong shape | `ask: arguments do not match the schema: <error>` |

## Cleaning

Every text field MUST be checked for valid UTF-8 before any other step. Invalid bytes MUST become U+FFFD.

A terminal escape sequence MUST be removed whole, with its parameters.

A control character MUST be removed. A bidirectional override character MUST be removed.

In a single-line field, a newline MUST become one space, and a tab MUST become one space.

In a preview, a newline MUST stay, and a tab MUST become four spaces.

A single-line field MUST lose its leading and trailing spaces. A preview MUST lose its leading blank lines and its trailing spaces.

The limits of the Arguments section MUST apply to the cleaned text.

## Results

The tool result MUST start with the line `User answers:` when the user answered at least one question.

An answered question MUST appear as one line in the form `- <header> (<question>): <answers>`.

Answers MUST be joined with `; `.

An answer that equals an option label MUST appear as the label.

An answer that is typed text MUST appear in double quotes. On a question with options, the quoted text MUST follow `Other: `.

A question with no answer MUST show `no answer`.

A question MUST NOT show more answers than one, unless it is a multi-select question. A multi-select question MUST NOT show more answers than its options plus one.

An answer MUST NOT exceed 2000 characters.

When every question has no answer, the result MUST be the skipped text below.

A cancelled question MUST return plain text. It MUST NOT return an error.

The fixed result texts MUST be these.

| Case | Text | Success |
|---|---|---|
| Every question skipped | The user skipped the questions and has no preference. Choose for yourself and say which choice you made. | yes |
| Dismissed | The user dismissed the questions without answering. Continue with your best judgment, or stop and say what you need if you cannot continue. | yes |
| No client | declined: no client could answer this question | no |
| Interrupted | interrupted: the user interrupted the turn before answering | no |
| Host restarted | interrupted: the host restarted before the user answered | no |
| Other failure | declined: followed by the failure detail | no |

## Host path

The host MUST offer each question to every subscribed client.

A question with no subscribed client MUST end at once with the no-client result.

A subscriber that joins MUST receive every open question, oldest first.

When the last subscriber leaves, the host MUST end every open question with the no-client result.

A window that cannot answer MUST count once for each question. The host MUST stop counting a window when it leaves.

A subscriber that the host closes as slow MUST NOT end the open questions, because the server subscribes again and receives them.

The first answer to a question MUST decide it. A later answer MUST fail with an unknown request error.

An interrupt of the turn MUST end the question as interrupted.

The host MUST mask the header, question, label, description and preview of each question before it offers the question.

The host MUST NOT journal a question or its answer.

The thread status MUST count the open questions.

## Scope

The `ask` tool MUST be available in top-level threads of `dal` and `dalgona`.

A subagent MUST NOT get the `ask` tool, because no user reads a subagent's questions.

An advisor thread MUST NOT get the `ask` tool, because an advisor waits for no person.

A call to `ask` in a subagent MUST complete with the error `Tool ask not found`.

Recovery MUST NOT ask an open question again. It MUST complete the call as interrupted, as the recovery table of [recovery.md](recovery.md) states.

The rule MUST be enforced by `withThreadTools` in `internal/agent/spawn.go`. `TestAskToolOnlyOnTopLevelThreads` and `TestAskToolInDal` in `internal/agent/ask_tool_test.go` check it.
