# steer: Steer the running turn or wake an idle one.

## Ported from
Original to dal; no pi counterpart. Pi nudges the session with `pi.sendUserMessage(text)`.

## What it shows
The `steer` tool takes `text` and checks `ctx.turn.is_idle().value`.
The `is_idle` reply is a tagged record; its `value` field holds the Boolean.
The tool calls `turn.wake` for an idle turn and `turn.steer` for a running turn.
It returns a message that states which path it took.
The command `/steer:steer` takes the text as its positional argument.

## What differs from pi
Pi sends a user message into the session unconditionally. This plugin branches on idleness first, because steering an idle turn and waking a running one are different requests to the host.

## Install
Copy to the dal plugins data root and add `steer` to `plugins`.

## Settings
None.
