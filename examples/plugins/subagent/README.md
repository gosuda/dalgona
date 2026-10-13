# subagent: Run tasks in separate dal child sessions.

## Ported from
pi `subagent`

## What it shows
Child sessions through `agents.start` and `agents.wait`. The `subagent` tool takes one `task`, or a `tasks` list of up to 8. It starts a child session for each task with that task as the prompt, waits for each in order, and returns the final answers joined by newlines. The child works in the same workspace with a fresh context.

## What differs from pi
The tool sends the child only the prompt text. Progress text from a running child is not shown. A tool cannot call another plugin's tool, so a child session is the only way to delegate.

## Install
Copy to the dal plugins data root and add `subagent` to `plugins`. Subagents are off by default in dal, so a session must allow them before the tool can start a child.

## Settings
None.
