# plan-mode: Toggle a plan-mode flag.

## Ported from
the shape of pi-plan-mode

## What it shows
A command that flips saved state. `/plan-mode:plan` reads the `plan-mode` key, writes `on` or `off` with the revision it read, and prints `plan mode: on` or `plan mode: off`.

## What differs from pi
The flag is only recorded. No hook reads it, because a `before_turn` hook is pure and cannot read state, so this plugin does not block edits and does not change the prompt. pi's plan mode does both. Enforcing the flag needs a host feature that does not exist in this release.

## Install
Copy to the dal plugins data root and add `plan-mode` to `plugins`.

## Settings
None.
