#### Spec-fix step 12. Light tool description states the REM display exception
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/hashline/hashline-common-v1.md` line 286 says: "Both descriptions MUST state the exception." Keep that sentence.

- Where: `internal/tool/patch/description.go:lightDescription`.
- Change: In the `lightDescription` string (description.go:11), after the sentence `REM/MV require the unchanged complete source file.` add the sentence `REM does not require every line to have been delivered.` before `All operations use the referenced before-image.` Why fix_code: the spec states a MUST about the model-facing description, and the code breaks it, so the sentence is kept and the code changes.
- Failing-first test: `TestLightDescriptionStatesRemExemption` in `internal/tool/patch/profile_test.go`. Input: description := New(string(HashlineLight)).Spec().Description (style value hashline-light, same call shape as TestSpecNames at profile_test.go:411-413) Expected: strings.Contains(description, "REM does not require every line to have been delivered.") is true; fails on current code because lightDescription lacks the sentence The expected value comes from: docs/spec/hashline/hashline-common-v1.md:286 after the TERM-8 edit (`neither profile requires every line to have been delivered for REM`) and the MUST at line 286 (`Both descriptions MUST state the exception.`)
- Evidence at commit `3c4b6f94`: internal/tool/patch/description.go:11 lightDescription has `REM/MV require the unchanged complete source file.` and no REM exemption; description.go:13 enhancedDescription has `REM is exempt from the every-line display rule.`
- Proof: the test fails before the change and passes after it, and `just verify` passes.

