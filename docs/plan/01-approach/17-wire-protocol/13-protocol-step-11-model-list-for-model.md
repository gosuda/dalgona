#### Protocol step 11. Model list for `/model`

- No `agent` wrapper. `modelList` (`serve_misc.go:53`) calls `model.PresetFor(provider)` (`internal/model/presets.go:22`, returns `(Preset, bool)`; `Preset` has `Default`, `Smol` and `Slow`) and lists `default`, `smol`, `slow` in that order for each role whose value equals the model's name without its provider prefix (`Host.Models` sets `Model.ID` to `<provider>/<model>`, `internal/agent/access.go:50`; split it with `model.ParseModelID` (`internal/model/model.go:116`) and compare the model part). `internal/rpc` may import `internal/model`; `.golangci.yml:139-150` (the `clients` rule) denies that import only to `internal/tui`, `internal/webui` and `internal/herdr`. The `fast` rule (`.golangci.yml:61-98`) denies neither `internal/model` nor `internal/rpc`.
- `internal/rpc/types.go` `Model`: `ContextWindow int json:"contextWindow"`, `Provider string json:"provider"`, `Presets []string json:"presets"`; `modelList` (`serve_misc.go:53`) fills them from `m.Context`, `provider(m.ID)` (`serve_thread.go:32`) and the preset lookup above (`model.PresetFor`, not `agent.PresetRoles`, per decision 21 of this workstream); `Model.write` writes them. The `inputModalities` member and the `Images` flag belong to the image-input workstream, Step 6. This step adds neither.
- Test `internal/rpc` `TestModelListCarriesContextAndPresets`: fake models `gpt-6.1-sol`, `gpt-test`, override `gpt-test` 50000; `openai/gpt-6.1-sol` has `presets ["default"]`, `provider "openai"`, `contextWindow 128000` (fallback, `internal/model/model.go:106-114`); `openai/gpt-test` has `contextWindow 50000`, `presets []`. Fails before: members absent.
- Docs: `protocol.md`, `docs/decisions/model-presets.md` (wire exposure line).

Anchor: Otl Aicher.

