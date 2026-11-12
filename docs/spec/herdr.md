# herdr integration

## Scope

The herdr reporter is OPTIONAL. It is active only inside a herdr pane.
The reporter MUST NOT block a turn when herdr is unavailable.
The client MUST bound each herdr command with a 5 s timeout.

## Activation

`FromEnv` MUST return `nil` unless `HERDR_ENV` is `1` and `HERDR_PANE_ID` is not empty.
The reporter MUST use `HERDR_BIN_PATH` when it is set. Otherwise it MUST run `herdr` from `PATH`.
The pane ID MUST come from `HERDR_PANE_ID`.

## Agent state

`State` MUST be one of `working`, `idle`, or `blocked`.
`Report` MUST run this command:

```text
herdr pane report-agent <pane> --source dalgona --agent dalgona --state <state> --seq <seq> --agent-session-id <thread> -- dalgona --resume <thread>
```

`seq` MUST be the journal head sequence. A status update MUST include the thread ID.
The command after `--` MUST let herdr resume the reported thread.

`Release` MUST run this command when the client exits:

```text
herdr pane release-agent <pane> --source dalgona --agent dalgona
```

## Subagent panes

`SubagentPane` MUST first run:

```text
herdr pane split --current --direction right --cwd <cwd> --no-focus
```

The command result MUST contain the string field `.result.pane.pane_id`.
The reporter MUST then run:

```text
herdr pane run <subagent-pane> "dalgona --resume <subagent-thread>"
```

The subagent command MUST be one argument after the pane ID.

## Failure behavior

A command failure MUST be logged with `log/slog`.
The reporter MUST log at most one failure for its lifetime.
A failed report, release, split, or subagent pane command MUST return control to the caller without an error.
A malformed split response MUST count as a command failure.
Cancellation MUST stop the command and MUST follow the same failure rule.

The reporter MUST parse the split response with a token decoder. It MUST NOT use reflection-based JSON decoding on this path.
