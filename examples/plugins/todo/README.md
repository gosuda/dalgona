# todo: Keep a todo list for this task.

## Ported from
pi `todo`

## What it shows
One tool with saved state. The `todo` tool takes an action (`list`, `add`, `toggle`, or `clear`), plus `text` for `add` and `id` for `toggle`. The list lives in the plugin's session state under the key `todos`. Each write passes the revision from the read before it, so a stale write fails with `conflict` instead of overwriting. The command `/todo:todos` prints the list.

## What differs from pi
pi keeps the list in the `details` of tool results and rebuilds it from the session. This plugin keeps it in saved state through `state.read` and `state.write`; an ephemeral session has no saved state. There is no custom rendering: the tool returns plain text.

## Install
Copy to the dal plugins data root and add `todo` to `plugins`.

## Settings
None.
