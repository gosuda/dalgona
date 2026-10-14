# session-log: Remember sessions and turns in state.

## Ported from
Original to dal; no pi counterpart.

## What it shows
Two stateful hooks plus a reader. The `session_start` hook records `start` and the `turn_end` hook records `turn` in the `session-log.json` state key. Both hooks call through `ctx.try_call`, so an ephemeral session records nothing and raises nothing. The list keeps the newest 50 entries. The `log` tool and the `/session-log:log` command print one marker per line.

`session_start` runs before a client attaches, so no one can answer a grant question then. The first open of a session records no `start`. If you approve the plugin's state grant for good rather than for one session, later opens record `start`.

## What differs from pi
Pi keeps this kind of journal in extension memory. Here the hooks persist it in plugin state, so a reopened session reads what earlier turns wrote.

## Install
Copy to the dal plugins data root and add `session-log` to `plugins`.

## Settings
None. State needs a saved session; an ephemeral session records nothing.
