#### Spec-fix step 9. Report a changed script effect replay as a mismatch
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/events.md` line 65 says: "a repeated index with a different `op` or `args_sha` MUST report the mismatch." Keep that sentence.

- Where: `internal/agent/event/state.go:foldScriptEffect`, `internal/agent/event/state.go:ScriptState`.
- Change: Change ScriptState.Seen (state.go:43) from map[uint64]struct{} to a map from effect index to a struct holding Op and ArgsSHA from the first effect. In foldScriptEffect (state.go:691-692), when the index is already present and Op or ArgsSHA differs, return `event: fold: script effect %s/%d mismatch` with the run id and index. A repeat with the same Op and ArgsSHA keeps the existing text `event: fold: duplicate script effect %s/%d`. Record the pair at state.go:697 and keep run.NextEffect at state.go:698 unchanged.
- Failing-first test: `TestFoldScriptEffectReportsMismatch` in `internal/agent/event/state_projection_test.go`. Input: turn.started t1; script.started run r1 with the entry, args, and seed the existing script fold tests use; script.effect {run_id r1, n 0, op read, args_sha A, result}; script.effect {run_id r1, n 0, op write, args_sha A, result} Expected: the second fold returns an error whose text is `event: fold: script effect r1/0 mismatch` The expected value comes from: Spec line 65 requires the mismatch to be reported. The error text is new and is set by this step. The field names Op and ArgsSHA come from internal/agent/event/event.go:533-534.
- Evidence at commit `3c4b6f94`: internal/agent/event/state.go:43 `Seen map[uint64]struct{}`; state.go:689 `return fmt.Errorf("event: fold: unexpected script effect %s/%d", e.RunID, e.N)`; state.go:691-692 `if _, seen := run.Seen[e.N]; seen {` then `return fmt.Errorf("event: fold: duplicate script effect %s/%d", e.RunID, e.N)`; state.go:697 `run.Seen[e.N] = struct{}{}`; spec line 55 says the state tracks the op and args_sha
- Proof: the test fails before the change and passes after it, and `just verify` passes.

