# plan-mode: Toggle a plan-mode flag.

## Ported from
the shape of pi-plan-mode

## What it shows
A command that flips saved state. `/plan-mode:plan` reads the `plan-mode` key, writes `on` or `off` with the revision it read, and prints `plan mode: on` or `plan mode: off`.

## What differs from pi
The flag is only recorded in session state. No hook reads it, because `before_turn` and `tool_call` hooks cannot read state under current plugin capabilities. As a result, this plugin does not block edits and does not inject plan guidance into the prompt. In an ephemeral session, state operations fail closed as unavailable.

## Install
Copy to the dal plugins data root and add `plan-mode` to `plugins`.

## Settings
None.
