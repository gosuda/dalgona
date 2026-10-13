# todo: Keep a todo list for this task.

## Ported from
pi `todo`

## What it shows
One tool with saved state. The `todo` tool takes an action (`list`, `add`, `toggle`, or `clear`), plus `text` for `add` and `id` for `toggle`:

- `list`: prints each item as `[ ] #<id>: <text>` or `[x] #<id>: <text>`, or `No todos` when the list is empty.
- `add`: requires non-empty `text`. Assigns an ID one greater than the highest existing ID and returns `Added todo #<id>: <text>`. Omitting `text` returns an error: `todo: text is required for add`.
- `toggle`: requires a nonzero `id`. Flips `done` and returns `Todo #<id> completed` or `Todo #<id> uncompleted`. Omitting `id` returns `todo: id is required for toggle`; an unknown ID returns `todo: #<id> not found`.
- `clear`: empties the list and returns `Cleared <n> todos`.

The list lives in session state under the key `todos`. Each write passes the revision from the read before it, so a stale write fails with a revision conflict instead of overwriting. The command `/todo:todos` prints the list.

## What differs from pi
pi keeps the list in the `details` of tool results and rebuilds it from the session. This plugin keeps it in saved state through `state.read` and `state.write`. In an ephemeral session, state operations fail closed as unavailable because no persistent store exists. There is no custom rendering: the tool returns plain text.

## Install
Copy to the dal plugins data root and add `todo` to `plugins`.

## Settings
None.
