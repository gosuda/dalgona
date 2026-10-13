# Rules: stream, always, and rulebook

Three kinds: stream rules watch output, always rules apply every turn, rulebook files hold project rules. Rule files live under the data root. Matching retries with backoff. Read a rule at `rule://<name>`. The judged-rule gate `auto|on|off` controls judge-fed rules; its start rule is stated here: `auto` asks the judge when cheap, `on` always asks, `off` never asks.
