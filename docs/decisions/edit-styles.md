# Edit styles

## Invariant

`anchor` MUST be the default edit style. `hashline-light` and `hashline-enhanced` MUST be selected only by the user's `edit_style` configuration; a model MUST NOT select a style. A refused Enhanced edit MUST NOT fall back to Light. The `patch` tool MUST reject symbol locators (`N*`).

## Scope

This decision applies to `internal/tool/patch` and the `edit_style` key in `internal/config`. [edit-styles.md](../spec/edit-styles.md) defines grammars and refusals. [The hashline README](../spec/hashline/README.md) lists the profile contracts.

## Enforcement

`TestHashlineRejectsSymbolOperations` in `internal/tool/patch/profile_test.go` checks rejection without a write. `just test` runs it.

## Rejected alternatives

- `hashline-enhanced` as the default. It lost because C18 of [the shared contract](../spec/hashline/hashline-common-v1.md) forbids a default change until conformance, legacy regression tests and paired end-to-end edit evaluations run, and [the conformance suite](../spec/hashline/hashline-conformance-v1.md) states that its product checks were not executed in that task.
- Symbol locators (`N*`). They lost because they need a language parser per file type, and line ranges from read output address the same text without one.
