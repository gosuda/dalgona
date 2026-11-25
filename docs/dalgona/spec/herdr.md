# herdr integration

## Scope

The herdr reporter is OPTIONAL. It is active only inside a herdr pane.
The reporter MUST NOT block a turn when herdr is unavailable.
The herdr caller MUST pass a context with a 5 s timeout to each reporter operation. `SubagentPane` MUST use that context for both commands.

## Activation

`FromEnv` MUST return `nil` unless `HERDR_ENV` is `1` and `HERDR_PANE_ID` is not empty.
The reporter MUST use `HERDR_BIN_PATH` when it is set. Otherwise it MUST run `herdr` from `PATH`.
The pane ID MUST come from `HERDR_PANE_ID`.

## Agent state

`State` MUST be one of `working`, `idle`, or `blocked`.
A report for the local endpoint MUST run this command:

```text
herdr pane report-agent <pane> --source dalgona --agent dalgona --state <state> --seq <seq> --agent-session-id <thread> -- dalgona --local --resume <thread>
```

`seq` MUST be the journal head sequence. A status update MUST include the thread ID.
The command after `--` MUST let herdr resume the reported thread.

A report for a remote endpoint MUST include its configured name in the restore command:

```text
herdr pane report-agent <pane> --source dalgona --agent dalgona --state <state> --seq <seq> --agent-session-id <remote-thread> -- dalgona --remote <name> --resume <thread>
```

The remote session identity MUST encode the remote name and thread ID without collisions.
Reports MUST follow only the focused endpoint.
The report sequence MUST belong to that endpoint and thread.
The caller MUST pass restore arguments separately, never forming a shell command from the remote name or thread ID.
Replacing a queued endpoint-and-thread report MUST move that key to the end of the queue.
The last delivered queued report MUST describe the latest focused endpoint.

`Release` MUST run this command when the client exits:

```text
herdr pane release-agent <pane> --source dalgona --agent dalgona
```

## Subagent panes

Automatic subagent panes MUST run only for the local endpoint.
A remote workspace path MUST NOT become the working directory of a local pane.

`SubagentPane` MUST first run:

```text
herdr pane split --current --direction right --cwd <cwd> --no-focus
```

The command result MUST contain the string field `.result.pane.pane_id`.
The reporter MUST then run:

```text
herdr pane run <subagent-pane> "dalgona --local --resume <subagent-thread>"
```

The subagent command MUST be one argument after the pane ID.

## Failure behavior

The reporter MUST log the first command failure with `log/slog`.
The reporter MUST NOT log any later failure for its lifetime.
A failed report, release, split, or subagent pane command MUST return control to the caller without an error.
A malformed split response MUST count as a command failure.
The reporter MUST stop the command when its context ends. The reporter MUST apply the same failure rule to that command.

The reporter MUST parse the split response with a token decoder. It MUST NOT use reflection-based JSON decoding on this path.
