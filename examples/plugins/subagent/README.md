# subagent: Run tasks in separate dal child sessions.

## Ported from
pi `subagent`

## What it shows
Child sessions through `agents.start`, `agents.wait`, and `agents.cancel`. The `subagent` tool takes a single `task` string, or a `tasks` list of 1 to 8 items. If neither is provided, it returns `no task`.

Single-task execution:
1. Starts one child session with `agents.start`, passing the task as the prompt. The child runs in the same workspace with a fresh context.
2. Awaits the child with `agents.wait` to collect its final report.
3. Closes the completed child with `agents.cancel`.
4. Returns the report text. If the host refuses to start the child (for example, when maximum child depth is reached), the tool returns `subagent: child refused: <reason>`.

Multi-task batches:
The handler attempts to start children in batches of up to 4 concurrent sessions before awaiting them. However, `start_child` supplies no explicit session name, so every child inherits the default plugin name (`subagent`). Because session names must be unique within a workspace, starting a second concurrent child is rejected by the host with a duplicate name error. The tool cleans up earlier started children in the batch with `agents.cancel` and returns the start error.

## What differs from pi
The child receives only prompt text; its progress is not streamed back. A tool cannot call another plugin's tool, so delegation uses a child session. Cancelling a parent turn during `agents.wait` closes the awaited child and its descendants. Before the wait begins, cancel started children explicitly rather than relying on turn cancellation. Closing the parent session closes its descendant sessions.

## Install
Copy to the dal plugins data root and add `subagent` to `plugins`. Subagents are off by default in dal, so a session must allow them before the tool can start a child.

## Settings
None.
