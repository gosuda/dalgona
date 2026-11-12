# Model presets

## Invariant

The program MUST NOT ship a default thread model. The user MUST select the thread model. A role preset MUST NOT grant access to a model that the provider does not offer.

`internal/model/presets.go` MUST hold the native provider identifiers in two tables: `Presets` for the role presets, and `presetCapabilities` for the capabilities of each identifier:

| Provider | default selection hint | smol | slow |
| --- | --- | --- | --- |
| `openai`, `chatgpt` | `gpt-6.1-sol` | `gpt-6-luna` | `gpt-6-astra` |
| `anthropic` | `claude-opus-5-5` | `claude-haiku-5-5` | `claude-fable-5-1` |
| `google` | `gemini-3.8-flash` | `gemini-3.5-flash-lite` | `gemini-4-argon` |
| Other providers | First offered id | First offered id | First offered id |

An explicit `[roles]` value MUST override the preset. An unavailable role preset MUST fall back to the thread model and log `roles.<name> preset <id> is not offered; using <model>`. The `advisor` role MUST resolve through its explicit role, the explicit `slow` role, the provider slow preset, and then the thread model. Advisor effort MUST be `medium`, clamped to the model's supported effort levels.

## Scope

This decision applies to model selection and role resolution, not provider entitlement. [providers.md](../spec/providers.md) and [advisor.md](../spec/advisor.md) define runtime behavior. The `default` column is a model-picker hint, not a configured thread model.

## Selection evidence

The approved selection record dated 2026-11-08 cites the native model guides for [OpenAI](https://developers.openai.com/api/docs/guides/latest-model.md), [Anthropic](https://platform.claude.com/docs/en/models/overview.md), and [Google](https://ai.google.dev/gemini-api/docs/models). It selects Sol over Astra for the same recorded Terminal-Bench score at a lower price. It selects Opus over Fable for the default role. These are the recorded design inputs, not new measurements from this repository.

| Recorded comparison | Selection evidence |
| --- | --- |
| [Terminal-Bench 4.0](https://snorkel.ai/leaderboard/terminal-bench/), page dated 2026-10-27 | Opus 5.5 with Claude Code at max effort: 64.8 +/- 3.1. Sonnet 5.5: 61.8 +/- 2.9. Astra with Codex: 58.2 +/- 2.8. Sol with Codex: 58.2 +/- 3.1. |
| [SWE-bench Pro public](https://labs.scale.com/leaderboard/swe_bench_pro) | Muse Spark 1.1: 61.50 +/- 3.10. GPT-5.4 at xHigh: 59.10 +/- 3.56. The record notes no run date and a table that predates GPT-6. |
| [SWE-bench Verified](https://openai.com/index/why-we-no-longer-evaluate-swe-bench-verified/) | The record excludes this benchmark after OpenAI stopped reporting it on 2026-02-23 because of contamination. |
| [OpenRouter programming usage](https://openrouter.ai/collections/programming/), data through 2026-11-07 | DeepSeek V4.1 Flash: 26.4%. GLM 5.3 Flash: 6.3%. Step 5 Preview: 6.1%. These figures measure traffic, not quality. |

The record cross-checks native names against [OpenRouter's catalog](https://openrouter.ai/api/v1/models). OpenRouter identifiers MUST NOT replace native identifiers on a provider wire. Subscription availability MUST still follow the provider's offered list. The record cites [ChatGPT model documentation](https://learn.chatgpt.com/docs/models/) and [Claude Code model aliases](https://code.claude.com/docs/en/model-config/), not an endpoint allowlist.

## Enforcement

`TestResolveRole` and `TestClampEffort` in `internal/model/model_test.go` check fallback and effort handling. `TestRoleResolutionUsesPresetsAndFallsBack` in `internal/agent/misc_test.go` checks host role resolution. `internal/config/advisor_test.go` checks advisor role inheritance. `just test` runs these tests. None of these tests proves that a live account offers a preset.
