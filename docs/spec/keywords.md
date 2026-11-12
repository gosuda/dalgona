# Magic keywords

## Keyword table

A `Keyword` MUST contain a word, a hue range, an optional required-tool list, an effort, a notice, and an enabled state.

`Builtins("dal")` MUST return only `ultrathink`. `Builtins("dalgona")` MUST return all four built-ins in this order.

| Word | Hue | Required tools | Effort |
| --- | --- | --- | --- |
| `ultrathink` | `[0, 330]` | none | `max` |
| `jevify` | `[300, 420]` | `eval` | `default` |
| `workflowz` | `[30, 150]` | `eval`, `task` | `default` |
| `orchestrate` | `[150, 280]` | `task` | `default` |

The package MUST embed every notice from `notices/*.md`. Every built-in MUST start enabled for its supported product.

## Custom keywords

`Merge` MUST apply custom entries in input order. A custom entry with the same word as an existing entry MUST replace that entry. An omitted notice, hue, effort, or required-tool list MUST inherit the existing value. An explicit empty required-tool list MUST replace the inherited list. A new custom entry MUST follow the existing entries.

A disabled entry MUST NOT appear in the merged table. This rule MUST allow `[keywords.ultrathink] enabled = false` to disable a built-in.

## Prose matching

`Match` MUST inspect only the supplied user prompt. It MUST NOT inspect synthetic messages or tool output. Matching MUST be case-sensitive. A keyword MUST appear in lowercase in both the table and the prompt.

The matcher MUST hide non-prose regions without changing rune positions. It MUST preserve newlines. It MUST support these regions:

- Fenced code blocks with at least three backticks or tildes. A fence MAY have up to three leading spaces. A closing fence MUST use the same marker and at least the opening length. An unclosed fence MUST hide the rest of the prompt.
- An opening backtick fence MUST NOT have a backtick in its info string.
- A closing fence MUST contain only whitespace after its marker.
- Inline code spans delimited by equal-length backtick runs. An unmatched run MUST remain literal text.
- XML and HTML comments, tags, and balanced element sections. Tag names MUST match without case sensitivity. Quoted `>` characters MUST NOT close a tag. An unclosed element MUST hide only its opening tag. An unclosed comment MUST hide the rest of the prompt.
- If another `<` appears before the tag ends, the first `<` MUST stay plain text.

`Match` MUST NOT return a keyword when a required tool is absent. `Match` MUST return each matching keyword at most once. Results MUST follow table order.

`Usable` MUST return the keywords that are enabled, lowercase, and whose required tools are all present.
`Previewable` MUST return the keywords that are enabled and lowercase, without checking required tools. A thread that has not resolved its tools MUST use `Previewable`.
`Spans` MUST return every non-overlapping prose occurrence of each keyword it receives, sorted by start.
Each span MUST give UTF-8 byte offsets, with the end offset exclusive.

## Boundaries

The rune before a match MUST NOT be a Unicode letter, a Unicode digit, `_`, `.`, `/`, `\`, or `-`. The two runes before a match MUST NOT be `::`.

The rune after a match MUST NOT be a Unicode letter, a Unicode digit, `_`, `/`, `\`, `-`, or `(`. A following `.` MUST NOT itself be followed by a Unicode letter, a Unicode digit, `_`, or `-`. Other punctuation MAY surround a match. Start and end of input MUST follow the same rules through an empty boundary rune.

## Notices

Notice text MUST use dalgona names: `eval`, `task`, `ctx.spawn`, `ctx.scope`, `ctx.wait`, `ctx.judge_batch`, and `yield` where relevant. Notice text MUST NOT use donor-only helper names or template directives.

Each sentence MUST use plain verbs and carry one instruction or fact. The notices MUST state these contracts:

- `ultrathink`: reason step by step before answering.
- `jevify`: freeze the rubric before reading data, pre-filter, judge the batch, read flagged or uncertain items, and report counts and confirmed items. Settled judgments MUST replay from the journal.
- `workflowz`: write durable `eval` phases, fan out, join subagents, contain item failures, and resume recorded effects without dispatching them again.
- `orchestrate`: decompose into `task` calls, require `yield`, verify each phase, and continue until no work remains.
