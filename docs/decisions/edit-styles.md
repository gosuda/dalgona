# Edit styles

## Invariant

`anchor` MUST be the default edit style. `hashline-light` and `hashline-enhanced` MUST be selected only by the user's `edit_style` configuration. A model MUST NOT select a style. A refused Enhanced edit MUST NOT fall back to Light. The `patch` tool MUST reject symbol locators (`N*`).

## Scope

This decision applies to `internal/tool/patch` and the `edit_style` key in `internal/config`. [edit-styles.md](../spec/edit-styles.md) defines grammars and refusals. [The hashline README](../spec/hashline/README.md) lists the profile contracts.

## Enforcement

`TestLoad_defaults` in `internal/config/config_test.go` and `TestStyleAliasesAndDefault` in `internal/tool/patch/property_test.go` check the `anchor` default. `TestPatchSchemaOffersNoStyleArgument` in `internal/tool/patch/profile_test.go` checks that no style schema takes a style argument. `TestHashlineRejectsSymbolOperations` in the same file checks rejection without a write. `TestEnhancedCoverageAndValidNoop` in the same file checks that a refused Enhanced edit writes nothing and does not fall back to Light. `just test` runs them.

## Rejected alternatives

- `hashline-enhanced` as the default. It lost because C18 of [the shared contract](../spec/hashline/hashline-common-v1.md) forbids a default change before five kinds of evidence exist. The evidence is conformance, legacy regression tests, platform faults and races, host resource measurements, and paired end-to-end edit evaluations. [The conformance suite](../spec/hashline/hashline-conformance-v1.md) states that its product checks were not executed in that task.
- Symbol locators (`N*`). They lost because they need a language parser per file type, and line ranges from read output address the same text without one.
