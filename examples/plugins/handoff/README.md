# handoff: Distill the session into a handoff note.

## Ported from
pi `handoff`

## What it shows
Child sessions, saved state, and tool execution together. The command `/handoff:handoff` has no positional parameters. Pass `--note=...`, or give the model tool `handoff` its optional `note` argument. The child receives a prompt for the goal, state, next steps, and open questions. Its default session name is `handoff`; the host refuses to start it if that name is already used in the workspace.

After the child finishes, the plugin saves its report under the session state key `handoff.md`. Success returns `Handoff written to handoff.md. Start the next session and ask for it.` Handoff requires durable session state. In an ephemeral session, the host refuses state access with `denied: state is unavailable`; no note is saved.

## What differs from pi
This plugin does not switch the parent session. It stores the note in the plugin's saved state. Child progress text is not shown. Cancelling a parent turn during `agents.wait` closes the awaited child and its descendants. Before the wait begins, cancel started children explicitly rather than relying on turn cancellation. Closing the parent session closes its descendant sessions.

## Install
Copy to the dal plugins data root and add `handoff` to `plugins`.

## Settings
None.
