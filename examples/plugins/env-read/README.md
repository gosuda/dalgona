# env-read: Read one environment variable.

## Ported from
Original to dal; no pi counterpart.

## What it shows
One tool over `env.read` and a plugin setting. The `read` tool takes a variable name, or falls back to the `key` setting from `[plugin.env-read]`. The host reads from the environment it captured at start. An unset variable returns an `env-read` error that names it. With no name and no `key` setting, the tool returns an error that says how to give one.

## What differs from pi
Pi reads `process.env` directly. Here the first read asks for the plugin's `env` grant, and the tool declares exactly one operation.

## Install
Copy to the dal plugins data root and add `env-read` to `plugins`.

## Settings
`key`: the default variable name when the tool gets none.
