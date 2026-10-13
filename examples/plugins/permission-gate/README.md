# permission-gate: Block risky tool calls by pattern.

## Ported from
pi `permission-gate`

## What it shows
A `tool_call` hook and plugin settings. The hook turns the call's arguments into text and blocks the call when the text contains `rm -rf`, `sudo`, or `force`, or a pattern you add. The block reason names the tool, the arguments, and the matching pattern. Any other call is allowed. The command `/permission-gate:gate` reports how many patterns are active.

## What differs from pi
A `tool_call` hook decides on its own: it can allow, block, or rewrite the call. It cannot ask you a question, because a deciding hook may request no operations. The match is a plain substring test, so it is a guard rail, not a security boundary.

## Install
Copy to the dal plugins data root and add `permission-gate` to `plugins`.

## Settings
`patterns` is an optional list of extra strings to block:

```toml
[plugin.permission-gate]
patterns = ["drop table"]
```
