# Providers

The model package MUST expose one normalized stream for every provider. A provider MUST NOT expose vendor types to the agent. Request and stream JSON MUST use `encoding/json/jsontext` token and value APIs. Reflection-based JSON coding is permitted only for cold model-list responses.

## Requests

A request MUST contain a model id, messages, and optional tools. A model id MUST use `<provider>/<model>`. Tool schemas MUST disable strict-mode validation. Missing context windows MUST default to 128000 tokens. Adapters MUST NOT select behavior with model-name conditionals.

The OpenAI provider MUST use `POST /responses`. `OPENAI_BASE_URL` MAY override its base URL. OpenAI function tools MUST use `type:function` with `strict:false`. An OpenAI freeform tool MUST use `type:custom`. Function history MUST use `function_call` and `function_call_output` items. Custom history MUST use `custom_tool_call` with text input and `custom_tool_call_output` for its result. Reasoning effort MUST use `reasoning.effort`, with summary `auto` and `reasoning.encrypted_content` included.

Anthropic MUST use `POST /messages`. Its system prompt MUST be an array of text blocks. The system block and the final block of the newest user message MUST carry `cache_control:{"type":"ephemeral"}`. The last tool definition MUST carry the same marker. Adaptive reasoning MUST use `thinking.type:adaptive`, display `summarized`, and `output_config.effort`. Effort MUST map minimal and low to `low`, medium to `medium`, and higher levels to `high`.

Chat-compatible providers MUST use `POST /chat/completions`. A non-empty request system prompt MUST be the leading `system` message. Assistant tool calls MUST precede their `tool` result messages. User text in a message that also carries tool results MUST follow those `tool` messages. Streaming requests MUST set `stream_options.include_usage:true`. Reasoning MUST use `reasoning_effort`.

Google MUST use `POST /v1beta/models/<model>:streamGenerateContent?alt=sse`. Tools MUST share one `functionDeclarations` array. JSON schemas MUST use `parametersJsonSchema`. A scalar `const` keyword MUST become a one-value `enum`. An object or array `const` MUST stay unchanged. A property named `const` under `properties`, `patternProperties`, `$defs`, `definitions`, or `dependentSchemas` is a name, not a keyword, and MUST stay unchanged. Function responses MUST use the tool name and an `output` or `error` string. Reasoning MUST use `thinkingConfig.includeThoughts:true` and a `thinkingLevel` of `MINIMAL`, `LOW`, `MEDIUM`, or `HIGH`.

Outside OpenAI, a freeform tool MUST remain a function tool. Its schema MUST contain one string property named `input`. Its arguments MUST have the form `{"input":"..."}`. The stream decoder MUST return the input text and set `Freeform`. An empty string is valid input. A missing or non-string `input` MUST fail the stream. Tool result output that is a JSON string MUST be sent as its unquoted text on every provider.

API keys MUST come from the caller callback or the provider environment variable. A configured base URL MUST replace the default endpoint.

## Streams

The stream MUST report text deltas, reasoning deltas, complete tool calls, usage, a stop reason, or an error. Tool arguments MUST preserve raw JSON when the provider supplies JSON. A decoder MUST NOT emit partial tool calls. Interleaved calls MUST retain separate ids, names, and argument buffers. Invalid complete JSON arguments MUST fail the stream.

OpenAI reasoning summaries and reasoning text MUST produce reasoning deltas. Anthropic `thinking_delta` blocks MUST produce reasoning deltas. Chat-compatible decoders MUST accept `reasoning_content`, `reasoning`, and `reasoning_text`, in that precedence order. A decoder MUST skip a reasoning field that is not a string and use the next field. Google parts with `thought:true` MUST produce reasoning deltas.

Usage MUST separate uncached input, cached reads, and cache writes. OpenAI and chat-compatible usage MUST subtract cache reads and writes from total prompt tokens. Chat-compatible cache reads MUST prefer `prompt_tokens_details.cached_tokens`, then `prompt_cache_hit_tokens`, then top-level `cached_tokens`, in any key order. Anthropic usage MUST merge non-null fields from `message_start` and `message_delta`. A null field MUST keep the earlier value. Google usage MUST subtract cached content from input and add thought tokens to output.

Stop reasons MUST use `stop`, `length`, or `toolUse`. Provider safety, refusal, unknown, and terminal error reasons MUST fail the stream. OpenAI refusal events and Google `promptFeedback.blockReason` MUST fail the stream. A stream without its required terminal event MUST fail. A stream that emitted a tool call MUST NOT retry. A stream failure MUST retain the raw event that caused it.

Google SSE data MAY contain one response object or an array of response objects. Only the first candidate MUST produce events.

The stream MUST stop when its context is canceled. A consumer MAY use `Next` or range over `Events`. The event queue MUST remain bounded.

## Capabilities and roles

`Models` MUST use the provider list endpoint and MUST follow its pagination until the provider reports no further page. Anthropic MUST page with `after_id` set to `last_id` while `has_more` is true. Google MUST page with `nextPageToken`. A repeated page cursor MUST fail. Each capability record MUST include the model id, context window, output limit, effort levels, and tool-choice support when known. Unknown numeric limits MUST remain zero, except for the context default. The context window MUST use the configured value for the bare model id first, then the provider-reported input limit, then 128000. Anthropic MUST read `max_input_tokens`, `max_tokens`, and the levels under `capabilities.effort`. Chat-compatible lists MUST read `context_length`, then `top_provider.context_length`, and `top_provider.max_completion_tokens`. Google MUST read `inputTokenLimit` and `outputTokenLimit` and skip models without `generateContent`. Other list endpoints MUST leave effort levels nil.

Static role presets MUST match the decision table. Providers without a static preset MUST use their first offered model for `smol` and `slow`. An unavailable preset MUST return the thread model and this exact log line: `roles.<name> preset <id> is not offered; using <model>`.

Effort clamping MUST choose the highest supported level at or below the request. It MUST choose the lowest supported level when every level exceeds the request. Nil effort support MUST omit the effort field.

## Retry and timeouts

Transport failures, 408, 429, and 5xx responses MUST be retryable. An HTTP 200 body or stream error with a numeric 429 or 5xx code MUST be retryable. Context-length errors MUST return `Overflow`. 401 and 403 MUST return `Auth`. Usage-limit and other errors MUST be terminal. Transient phrases such as `timeout` MUST be matched only against local transport error text, never against a provider body, so model text inside a refusal cannot cause a retry.

A request MUST retry at most three times. Delays MUST be one, two, and four seconds with plus or minus twenty percent jitter. A valid `Retry-After` value up to 60 seconds MUST replace the delay. The retry callback MUST run before sleeping.

The first-event deadline MUST cover connection, response headers, and the first SSE event. It MUST stop after the first event. The idle deadline MUST measure received bytes. An idle value of zero MUST disable the idle deadline. The idle controller MUST support pause and resume for in-band tool execution. Timeout failures MUST be retryable. Errors MUST retain HTTP status and raw provider error bodies when available.
