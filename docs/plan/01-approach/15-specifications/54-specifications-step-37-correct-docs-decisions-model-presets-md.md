#### Specifications step 37. Correct `docs/decisions/model-presets.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/decisions/model-presets.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 7 | `internal/model/presets.go` MUST hold the native provider identifiers in one table: | `internal/model/presets.go` MUST hold the native provider identifiers in two tables: `Presets` for the role presets, and `presetCapabilities` for the capabilities of each identifier: | internal/model/presets.go:15 `var Presets = map[string]Preset{`; internal/model/presets.go:30 `var presetCapabilities = map[string]Model{`; internal/model/model_test.go:118-122 `for provider, preset := range Presets {` ... `capability, ok := Capabilities(provi |

Structure:

- Role table: presets.go:16-19 holds openai, chatgpt, anthropic and google with the same default, smol and slow identifiers as the doc table.
- Fallback message: presets.go:70 returns fmt.Sprintf('roles.%s preset %s is not offered; using %s'); misc_test.go:153-156 checks the logged text.
- Offered check: the preset is used only when offeredModel is true (presets.go:66-68), so a role preset does not grant an unoffered model.
- Advisor role chain: runtime.go:134 uses cmp.Or(spec.Model, cfg.Roles.Configured('advisor'), 'advisor'); config/advisor.go:94-95 maps advisor to cmp.Or(r.Advisor, r.Slow); presets.go rolePreset (lines 73-85) maps advisor and slow to preset.Slow; ResolveRole falls back to the thread model (presets.go:70).
- Thread model: host.go:265-266 returns 'agent: no model is configured; pick one with /model' when created.Model is empty, so no default thread model ships.
- Default column: no non-test reader of Preset.Default exists in internal/ or cmd/ (grep this run). PresetFor (presets.go:22) has no caller, and model_test.go:119 is the only reader of .Default. The doc's model-picker hint has no code consumer yet. The code does not contradict the sentence, so it stays.
- OpenRouter: presets.go has no openrouter key. OpenRouter appears only as a provider in auth/auth.go:37-41 and as a decision endpoint in decide (grep this run).

Do not rewrite the rest of this file. Proof: `just verify` passes.

