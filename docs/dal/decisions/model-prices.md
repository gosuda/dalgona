# Model prices

## Invariant

The server MUST derive the thread cost on read from token counts and a price table. The journal MUST NOT record a cost.

A price MUST be a whole number of micro-USD per million tokens. The loader reads each TOML number once, rounds it, and keeps no float. The wire carries an integer count of micro-USD.

A configured price MUST win over a built-in price. A model with no price adds no cost. The thread cost is partial when a priced message and a message on an unpriced model both exist.

`presetPrices` in `internal/dal/model/price.go` MUST hold the built-in rows. A row MUST name a model of `presetCapabilities`. A preset with no verified price MUST have no row.

## Scope

This decision covers the price table, the rounding rule, and the built-in rows. [providers.md](../spec/providers.md#cost) and [config.md](../spec/config.md) define runtime behavior.

The cost covers the assistant messages of the thread. It does not cover advisor, decider, compaction, or subagent spend.

Each row uses the base price tier of its model. A prompt over a long-context threshold costs more at the provider, and the table does not model that tier. The cost of such a prompt is an underestimate. The thresholds are 272,000 tokens for the OpenAI rows and 100,000 tokens for `claude-haiku-5-5`.

The `chatgpt` presets have no row. A ChatGPT subscription bills no price per token.

## Price evidence

The read date of every page below is 2026-11-15. Each value is US dollars per million tokens.

| Model | Input | Output | Cache read | Cache write |
| --- | --- | --- | --- | --- |
| `openai/gpt-6.1-sol` | 2.00 | 10.00 | 0.10 | 2.50 |
| `openai/gpt-6-luna` | 0.10 | 0.50 | 0.01 | 0.125 |
| `openai/gpt-6-astra` | 10.00 | 50.00 | 1.00 | 12.50 |
| `anthropic/claude-opus-5-5` | 4.00 | 20.00 | 0.20 | 5.00 |
| `anthropic/claude-haiku-5-5` | 0.10 | 0.50 | 0.01 | 0.125 |
| `anthropic/claude-fable-5-1` | 10.00 | 50.00 | 0.25 | 12.50 |
| `google/gemini-3.8-flash` | 0.75 | 3.75 | 0.075 | 0.75 |
| `google/gemini-3.5-flash-lite` | 0.30 | 2.50 | 0.03 | 0.30 |
| `google/gemini-4-argon` | 2.00 | 10.00 | 0.10 | 2.00 |

| Provider | Page | Rows read |
| --- | --- | --- |
| OpenAI | [API pricing](https://developers.openai.com/api/docs/pricing), Standard table, short context | Input, output, cached input, and the 30 minute cache write. |
| OpenAI | [Prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching) | The default cache lifetime is 30 minutes, so the write rate is the 30 minute rate. |
| Anthropic | [Pricing](https://platform.claude.com/docs/en/about-claude/pricing), model pricing table | Base input, 5 minute cache write, cache hit, and output. The adapter sends the default cache lifetime. |
| Google | [Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing), Standard paid tier | Input, output, and context caching. Implicit caching has no write charge, so the write rate is the input rate. |

Two Google rows change on a known date. `google/gemini-4-argon` costs 4.00 input, 20.00 output, and 0.20 cache read from 2026-12-01. `google/gemini-3.8-flash` costs 1.50 input, 7.50 output, and 0.15 cache read from 2027-01-01. A user MAY override a row with `[models."<id>"]` before the owner updates the table.

## Enforcement

`TestCostMicros`, `TestCostMicrosRoundsHalfUpOnce`, and `TestCostMicrosSaturatesInsteadOfWrapping` in `internal/dal/model/price_test.go` check the arithmetic. `TestPresetPricesNameKnownPresetModels` in the same file checks that each row names a preset model.

`TestConfigPriceErrors` and `TestConfigPriceAcceptsBoundariesAndLoadsMicros` in `internal/dal/config/price_test.go` check the keys and the rounding. `TestUsageCostPartial` and `TestTokenUsageCarriesCostOnEveryWirePath` in `internal/dal/rpc/serve_cost_test.go` check the wire. `just test` runs these tests.

None of these tests proves that a built-in row matches a provider page today. The evidence table above is the only record of that match.

## Rejected alternatives

- Floating-point dollars on the wire. A client that adds them drifts. The wire carries integer micro-USD.
- A cost in the journal. A new price table would leave old threads wrong, and recovery would need a new row. The cost derives on read.
- A live price fetch. It would put a network call in the tracker and differ on replay.
- A row for every offered model. A row without a verified page value would be a guess.
