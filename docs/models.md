# Providers, models, and thinking levels

Four API families: OpenAI Chat Completions, OpenAI Responses, OpenAI Codex with the ChatGPT sign-in, Anthropic. Run `dalgon login`, then pick a model. First run lists models; aliases live in config. Thinking levels map per API and clamp to model limits; temperature requires thinking to be off. Once delivered semantic output begins, in-stream transport cuts do not retry; pre-delivery transport cuts retry. Luna Reserve holds prepaid balance. Codex sends `originator: dalgon`; Claude shapes requests byte for byte as the reference harness does.
