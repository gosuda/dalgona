# Starlark replay

## Invariant

A durable script MUST journal its source, entry point, arguments, and random seed before it runs. Each external effect MUST have a stable submission index and argument hash. Replay MUST return a recorded effect result instead of repeating the effect. A changed operation or argument hash MUST fail replay.

Pure script evaluation MAY run again after a restart. Tool calls, waits, judgments, time, and random values MUST use the durable run context. A completed run MUST return its recorded outcome. Replay MUST NOT repeat a completed external action.

## Scope

This decision applies to `internal/script`, the eval tool, workflows, plugins, and hooks. [events.md](../spec/events.md) defines `script.started`, `script.effect`, and `script.completed`. [recovery.md](../spec/recovery.md) requires script-class tools to reopen their own durable runs under the same call id.

## Enforcement

`internal/agent/event/state_test.go` checks the fold of out-of-order script effects. `internal/script/stdlib/stdlib_test.go` checks injected time and random sources and the required dialect. These tests do not prove runtime replay. Runtime integration MUST test a crash after an effect, a restart without a second effect, and refusal on an operation or argument mismatch before this decision can be reported as enforced end to end.
