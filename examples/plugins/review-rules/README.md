# review-rules: Stream and always-apply rules for review.

## Ported from
Original to dal; no pi counterpart. Pi has no rule surface; dalgona documents project rules separately.

## What it shows
The `dal.rule` constructor in both shapes. The `no-todo` rule watches model output for the `TODO` pattern and answers with its text on a match. The `always-test` rule sets `always_apply = True` and applies every turn with no pattern. The prompt section adds one fixed review instruction to the system prompt.

## What differs from pi
There is nothing to translate. A rule needs a pattern unless it always applies; both forms live side by side in one `rules` map.

## Install
Copy to the dal plugins data root and add `review-rules` to `plugins`.

## Settings
None.
