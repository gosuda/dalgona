# Automated approval

## Invariant

Approval mode `auto` MUST use mechanical rules and an automated decider. It MUST NOT wait for a user. User approval MUST occur only in `ask`. Mode `yolo` MUST bypass the decider and disable the harness sandbox.

The gate MUST apply hooks, mode handling, mechanical allows, static refusals, and the selected decision path in the order in [approvals.md](../spec/approvals.md). A static refusal MUST NOT reach the decider. A missing or failed decider MUST decline. An advisor MUST NOT approve or reject an action.

## Scope

This decision applies to primary, subagent, script-inner, remote-tool, and advisor investigative calls that enter the agent gate. Config MUST put the mode in `approval` and the decider settings in `[approval_decider]`. [config.md](../spec/config.md) defines that TOML shape. [decide.md](../spec/decide.md) defines adapter answers.

## Enforcement

`internal/agent/approval_test.go` checks score thresholds, static refusals, decider timeouts, `yolo`, thread grants, and `ask` without a client. `internal/config/config_test.go` checks mode validation and rejects the old approval table. `just test` runs these tests.
