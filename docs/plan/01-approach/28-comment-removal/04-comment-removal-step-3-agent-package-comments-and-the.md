#### Comment removal step 3. Agent package comments and the CheckRemoteValue rename

Anchor: Tony Hoare

- Files: `internal/agent/decider.go`, `internal/agent/effective_sandbox.go`, `internal/agent/rules/rules.go`, `internal/agent/gate.go` (one call), `internal/agent/workspace_trust.go`, `docs/spec/approvals.md`, `docs/spec/mcp.md`.
- Change: delete the comment lines of rows 23 to 35. Rename `rules.CheckValue` to `rules.CheckRemoteValue` at `internal/agent/rules/rules.go:165` and at the call in `internal/agent/gate.go:154`. The doc comment at `rules.go:164` (row 27) goes in the same commit. Rows 23 and 30 also add the spec text below.
- Spec text, `docs/spec/approvals.md`: as a new paragraph at the end of `## Decider adapter`, after its last bullet: `NewFactoryDecider` MUST resolve its decide client for the thread being judged on each action, then score through the same `Decide` path. A `ClientFactory` MUST return the decide client and model for the thread it receives.
- Spec text, `docs/spec/mcp.md`: as a new paragraph at the end of `## Workspace trust`: A workspace trust caller MUST refuse the workspace when the request returns an error, unless the reply is cancel. A workspace trust request MUST decline when no client subscribes to the thread.
- Tests that fail before the change: none. The rename is compile-checked by `just vet`.
- Check: `just verify` passes. `grep -rn "CheckValue" internal cmd docs` returns no line.
- Lands after: Comment removal step 2

