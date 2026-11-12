# Advisor

## Invariant

The advisor MUST remain an optional transcript reviewer in both products. It MUST NOT approve actions. It MUST NOT change the primary session except by delivering a note. `WATCHDOG.md` guidance MUST remain in the advisor prompt and MUST NOT enter the primary prompt.

Each advisor MUST use its own durable child session. Clients MUST NOT list it as a peer thread. Delivered notes MUST NOT return as review input. A user interrupt MUST prevent automatic continuation until the next user input. One note batch MUST start at most one continuation.

## Scope

This decision applies to `internal/agent/advisor`, advisor config, journal events, and protocol delivery. [advisor.md](../spec/advisor.md) defines cadence, model selection, tools, note guards, delivery, and recovery. [recovery.md](../spec/recovery.md) reserves advisor recovery for the advisor runtime.

## Enforcement

Tests in `internal/agent/advisor/guard_test.go`, `render_test.go`, `route_test.go`, and `watchdog_test.go` check deduplication, note filtering, the delivery matrix, and advisor-only guidance. `internal/agent/event/advisor_test.go` checks event validation and duplicate delivery refusal. `internal/config/advisor_test.go` checks both product configs. These checks MUST NOT be treated as proof of crash recovery or end-to-end runtime delivery. Those paths require runtime integration tests. `just test` runs the existing tests.

## Rejected alternatives

- The advisor on by default. It lost because each review adds a model request per reviewed turn, and the user should choose that cost with `advisor.enabled = true` or `/advisor`.
