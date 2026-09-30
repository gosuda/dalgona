# handoff: Distill the session into a handoff note.

## Ported from
pi `handoff`

## What it shows
Child sessions, saved state, and `ctx.try_call` together. `/handoff:handoff` starts a child session with a prompt that asks for the goal, state, next steps, and open questions, plus your optional note. It waits for the child, then saves the child's report as the state key `handoff.md`. It reads and writes that key through `ctx.try_call`, so an ephemeral session, where state is `unavailable`, gets a short message instead of a failure.

## What differs from pi
This plugin does not create or switch sessions. It stores the note in the plugin's saved state and reports that it did. Progress text from the child is not shown.

## Install
Copy to the dal plugins data root and add `handoff` to `plugins`.

## Settings
None.
