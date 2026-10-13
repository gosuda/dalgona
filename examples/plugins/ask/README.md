# ask: Ask the user several questions in a row.

## Ported from
pi `questionnaire`

## What it shows
Questions to the user through `ask.select` and `ask.text`. The `questionnaire` tool takes 1 to 8 questions, each with an `id`, a `prompt`, and optional `options` (2 to 10 strings). A question with options is a choice list with one extra choice, "Type an answer". A question without options is a text prompt. The tool returns one `id: answer` line per question, and a blank answer reads `no answer`. The `ask` operations need to be declared in `uses` but need no grant.

## What differs from pi
Each question is one modal prompt, asked in order. There is no form or custom screen. The schema accepts an `allow_other` field, but the handler does not read it.

## Install
Copy to the dal plugins data root and add `ask` to `plugins`.

## Settings
None.
