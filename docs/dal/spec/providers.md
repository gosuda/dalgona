# Providers

The model package MUST expose one normalized stream for every provider. A provider MUST NOT expose vendor types to the agent. Request and stream JSON MUST use `encoding/json/jsontext` token and value APIs. Reflection-based JSON coding is permitted only for cold data that the program reads once, such as model-list and control responses.

## Requests

A request MUST contain a model id, messages, and optional tools. A model id MUST use `<provider>/<model>`. Tool schemas MUST disable strict-mode validation. Missing context windows MUST default to 128000 tokens. Adapters MUST NOT select behavior with model-name conditionals.

The OpenAI provider MUST use `POST /responses`. `OPENAI_BASE_URL` MAY override its base URL. OpenAI function tools MUST use `type:function` with `strict:false`. An OpenAI freeform tool MUST use `type:custom`. Function history MUST use `function_call` and `function_call_output` items. Custom history MUST use `custom_tool_call` with text input and `custom_tool_call_output` for its result. Reasoning effort MUST use `reasoning.effort`, with summary `auto` and `reasoning.encrypted_content` included. The OpenAI adapter MUST send each image as an `input_image` item with a data URL. The OpenAI adapter MUST send `detail` as `auto`.

Anthropic MUST use `POST /messages`. Its system prompt MUST be an array of text blocks. The system block and the final block of the newest user message MUST carry `cache_control:{"type":"ephemeral"}`. The last tool definition MUST carry the same marker. Adaptive reasoning MUST use `thinking.type:adaptive`, display `summarized`, and `output_config.effort`. Effort MUST map minimal and low to `low` and MUST pass `medium`, `high`, `xhigh`, and `max` through unchanged. The Anthropic adapter MUST send each image as a base64 `image` block before the text block of its message. With an Anthropic OAuth credential, requests MUST carry `Authorization: Bearer` and `anthropic-beta: claude-code-20250219,oauth-2025-04-20`.

Chat-compatible providers MUST use `POST /chat/completions`. A non-empty request system prompt MUST be the leading `system` message. Assistant tool calls MUST precede their `tool` result messages. User text in a message that also carries tool results MUST follow those `tool` messages. Streaming requests MUST set `stream_options.include_usage:true`. Reasoning MUST use `reasoning_effort`. The chat-compatible adapter MUST send each image as an `image_url` part with a data URL.

Google MUST use `POST /v1beta/models/<model>:streamGenerateContent?alt=sse`. Tools MUST share one `functionDeclarations` array. JSON schemas MUST use `parametersJsonSchema`. A scalar `const` keyword MUST become a one-value `enum`. An object or array `const` MUST stay unchanged. A property named `const` under `properties`, `patternProperties`, `$defs`, `definitions`, or `dependentSchemas` is a name, not a keyword, and MUST stay unchanged. Function responses MUST use the tool name and an `output` or `error` string. Reasoning MUST use `thinkingConfig.includeThoughts:true` and a `thinkingLevel` of `MINIMAL`, `LOW`, `MEDIUM`, or `HIGH`. Effort MUST map `minimal` to `MINIMAL`, `low` to `LOW`, and `medium` to `MEDIUM`. Effort `high`, `xhigh`, and `max` MUST map to `HIGH`. The Google adapter MUST send each image as an `inlineData` part with a `mimeType` and base64 `data`. The Google adapter MUST place image parts before the text part of their message.

Outside OpenAI, a freeform tool MUST remain a function tool. Its schema MUST contain one string property named `input`. Its arguments MUST have the form `{"input":"..."}`. The stream decoder MUST return the input text and set `Freeform`. An empty string is valid input. A missing or non-string `input` MUST fail the stream. Tool result output that is a JSON string MUST be sent as its unquoted text on every provider.

API keys MUST come from the caller callback or the provider environment variable. A configured base URL MUST replace the default endpoint.

## TLS roots

A non-empty `tls_ca_file` setting MUST install its PEM certificates into the process default HTTP transport before any client exists. The install happens once, right after the product loads its configuration. Model HTTP clients built with `NewHTTPClient(nil)` use the default transport. OAuth token requests, the MCP HTTP transport, the `chatgpt` transport, and the decision service follow the default transport. The Cloudflare service, the remote-bridge websocket dial, and `webfetch` follow it as well. The Docker client, the Modal client, and the web server certificate keep their own trust settings.

The root pool comes from `x509.SystemCertPool` with the configured certificates appended. Appended roots add trust on Windows, macOS, and iOS, because platform verification keeps its authority over such a pool. An inherited `SSL_CERT_FILE` or `SSL_CERT_DIR` variable makes Go 1.27 read the platform roots from disk instead. Set `GODEBUG=x509sslcertoverrideplatform=0` to restore the platform roots.

## Subscription and IDE providers

OAuth adapters MUST take their credential from a caller callback. `oauth.Source` MUST load the stored token once. It MUST refresh the token through `oauth.Refresh` when the token expires within 60 seconds. A token without an expiry MUST NOT refresh. Concurrent callers MUST share one refresh. A refresh MUST keep the previous account when the response carries none. The source MUST call `Save` after each successful refresh. A refresh failure MUST return an error and MUST keep the previous token for the next call. A save failure MUST return an error and MUST keep the refreshed token, because the refresh grant MAY have revoked the previous refresh token. The next call MUST retry the save without a new refresh.

A token that expires without a refresh token MUST return the error `<provider> token expired and has no refresh token; log in again`.

### Copilot

The `copilot` provider MUST use chat-compatible requests at `https://api.githubcopilot.com`.
It MUST exchange the GitHub credential at `GET https://api.github.com/copilot_internal/v2/token`.
The exchange MUST send `Authorization: Token <github>`.
Concurrent callers MUST share one exchange.
The cached token expires 60 seconds before `expires_at`.
API requests MUST send `Authorization: Bearer <copilot token>`.
Both editor headers MUST identify `dalgona/<version>`.
`Copilot-Integration-Id` MUST be `vscode-chat`.
`Openai-Intent` MUST be `conversation-panel`.
Each request MUST have a fresh `X-Request-Id`.
`X-Initiator` MUST be `agent` when the last message contains tool results.
Otherwise it MUST be `user`.
The model list MUST keep only unique ids with `model_picker_enabled` true.
It MUST read context limits, output limits, and declared reasoning efforts.
Copilot MUST report no image support.
Missing context limits MUST use the configured limit or 128000.
Copilot has no static role preset.
Exchange refusals MUST NOT trigger login, credential rotation, identity changes, fallback, or restriction retries.
An exchange 401 MUST ask the user to inspect the account.
An exchange 403 or 404 MUST ask the user to check access and service availability.
Errors MAY include a bounded, masked provider message.
Live sign-in and acceptance of the editor identity remain Not proven.

### Other subscription providers

The `chatgpt` provider MUST send the OpenAI Responses request to `https://chatgpt.com/backend-api/codex/responses`. Each request MUST carry `Authorization: Bearer <access>`, `chatgpt-account-id`, `OpenAI-Beta: responses=experimental`, and `originator: codex_cli_rs`. A token without an account id MUST fail before the request is sent. An empty system prompt MUST become `You are a helpful assistant.`. Stream decoding MUST follow the OpenAI rules. The ChatGPT adapter MUST send images through the OpenAI Responses encoder.

The ChatGPT account id follows the ChatGPT account extraction rule in [oauth.md](oauth.md).

The default `chatgpt` base URL MUST be `pi`'s `DEFAULT_CODEX_BASE_URL` with `/codex` appended.

`Models` MUST discover the released client version from `https://registry.npmjs.org/@openai%2Fcodex/latest`.
The version cache MUST retain `version`, `etag`, and `checked_at` in `codex-client-version.json`.
A valid entry younger than four hours MUST avoid a registry request.
A stale entry MUST send its ETag during revalidation.
A 304 MUST renew only a valid cached entry.
A 200 MUST supply three numeric version components and MUST NOT supply `0.0.0`.
A failed lookup MUST retain a valid cached version.
Without one, discovery MUST return `chatgpt: client version is unavailable: <cause>` before requesting models.
Discovery failure MUST NOT change existing thread or model settings.

`Models` MUST request `<base>/models?client_version=<version>` through the authenticated account transport.
Each discovery request MUST have a five-second timeout and a one-MiB response limit.
Discovery MUST NOT follow redirects.
The list MUST retain unique, non-empty slugs with `visibility` equal to `list`.
It MUST sort by ascending priority and preserve source order for ties.
It MUST read context limits, output limits, and declared reasoning efforts.
A valid effective context percentage MUST scale the reported context limit.
Configured context limits MUST override discovered limits.
Image support MUST be true when `input_modalities` contains `image` or is absent.
Tool choice MUST be true.

The catalog cache MUST use `codex-models.json` with mode 0600 and replacement through a temporary file.
Its identity MUST combine the canonical provider endpoint and authenticated account id.
It MUST NOT contain access tokens.
Account changes MUST invalidate the in-memory catalog before discovery continues.
Foreign or corrupt cached catalogs MUST be ignored.
Transport failures and server 5xx responses MUST use the matching cache, or the static offline list.
The offline list retains the Codex donor ids, context limits, effort levels, and image support.
The static offline list MUST take its model values from Codex `codex-rs/models-manager/models.json`.
An outage warning MUST appear at most once per hour for each identity.
Authentication, access, quota, rate-limit, and account-restriction responses MUST return errors without catalog fallback.
Discovery MUST NOT rotate credentials, switch providers, or retry restrictions.
A missing catalog entry MUST NOT grant access or bypass a provider refusal.
The `model/list` wire shape remains unchanged.

The `antigravity` provider MUST send `POST /v1internal:streamGenerateContent?alt=sse` to `https://daily-cloudcode-pa.googleapis.com`. The body MUST be the envelope `{project, requestId, request, model, userAgent:"antigravity"}`. `request` MUST be the Google request body. `requestId` MUST have the form `agent/<id>/<unix ms>/<id>/<step>`, where `<step>` is the message count and at least 1. The `User-Agent` header MUST be `antigravity/hub/<version> (aidev_client; os_type=<os>; arch=<arch>; cl=<changelist>)`. Each SSE event MUST carry the Google response under `response`. An event without `response` MUST decode as a Google response. Decoding MUST follow the Google rules.

The Antigravity client headers MUST follow oh-my-pi's `packages/catalog/src/wire/gemini-headers.ts`, with the offline fallback version `2.26.0`. The `requestId` form MUST follow `buildAntigravityRequestEnvelope` in oh-my-pi's `packages/ai/src/providers/google-gemini-cli.ts`. The static Antigravity list MUST name the `google-antigravity` models with thinking mode `google-level` in oh-my-pi's `packages/catalog/src/models.json`. The onboarding tier and timeout MUST follow oh-my-pi's `packages/ai/src/registry/oauth/google-antigravity.ts`.

Antigravity MUST resolve its project once per provider and reuse it. A configured project MUST skip discovery. Discovery MUST call `loadCodeAssist` with `metadata.ideType=ANTIGRAVITY`. When the response has no paid tier and names a project, discovery MUST call `loadCodeAssist` again with that project. A response that does not allow `free-tier` and gives an ineligibility reason for it MUST fail with that reason and its validation URL. A response without `currentTier` MUST start `onboardUser` with `tierId=free-tier`. Discovery MUST poll the returned operation once per second until it is done, for at most 30 seconds. A done operation with an error or without a response MUST fail. Discovery MUST then call `loadCodeAssist` again and MUST fail when no project is returned. `Models` MUST return the static donor list of Gemini models that use thinking levels. A model with an effort route MUST send the routed wire model for the requested effort, else its default wire model.

The `devin` provider MUST use Connect on `https://server.codeium.com`. The credential is `DEVIN_API_KEY` or the Devin OAuth token. `GetUserJwt` MUST first send the credential with the `devin-session-token$` prefix. After an HTTP 401 it MUST send the raw credential. The provider MUST cache the JWT until 60 seconds before its `exp` claim. A JWT without `exp` MUST stay cached until a chat request returns 401. After a 401 from a cached JWT, the provider MUST exchange a new JWT and repeat the request once.

Devin chat MUST send one gzip-compressed Connect frame to `GetChatMessage`. The request MUST use request type 5 and planner mode 1. The request MUST set tool choice `auto`, an ephemeral system prompt cache, and `DisableToolCalls` to true. The metadata MUST identify `devin-cli` `3000.11.3` with IDE type `chisel` and MUST carry the JWT. User text MUST use source 1. Assistant turns MUST use source 2, with reasoning text placed before the visible text. Tool results MUST use source 4 and precede user text of the same message. Tool call ids MUST keep only `[A-Za-z0-9_-]`, MUST be cut to 40 characters, and MUST gain a `_dupN` suffix on collision. A call and its result MUST map to the same wire id.
The Devin adapter MUST send each image in the `Images` field of its user prompt.

Devin response frames MAY be gzip-compressed. A decompressed frame larger than 16 MiB MUST fail. Tool call deltas MUST join by id. A delta without an id MUST extend the most recent call. A delta that repeats the previous arguments as its prefix MUST replace them. The decoder MUST emit tool calls only after a clean end of stream. Usage counters MUST keep their maximum over the stream. Stop reason 3 MUST map to `length`. Any tool call MUST map to `toolUse`. Reasons 0, 2, and 10 MUST map to `stop`. Other reasons MUST fail. An end-of-stream trailer with an error MUST fail with the HTTP status of its Connect code. A stream that ends without an end-of-stream frame and without a stop reason MUST fail. `Models` MUST call `GetCliModelConfigs` and MUST skip disabled, duplicate, and router models.

The `devin` Connect error map MUST follow the Connect protocol error table at `https://connectrpc.com/docs/protocol/#error-codes`. The `devin` stop reason numbers MUST come from the `stop_reason` enum in `engine/dal-provider/src/family/devin/proto.rs` of the dal repository at commit `f2cb47b8`. The `devin` request constants MUST come from `engine/dal-provider/src/family/devin/request.rs` and `proto.rs` of the dal repository at commit `f2cb47b8`.

The `cursor` provider MUST open one HTTP/2 Connect stream to `POST https://api2.cursor.sh/agent.v1.AgentService/Run` per attempt. The request body MUST stay open for client frames while the response streams. The first frame MUST be the run request. The client MUST send a heartbeat frame every five seconds. Requests MUST carry `Authorization: Bearer <access>`, `x-ghost-mode: true`, `x-cursor-client-type: cli`, the donor client version, and a fresh `x-request-id`.

The `cursor` provider MUST map Connect errors with the Connect protocol error table at `https://connectrpc.com/docs/protocol/#error-codes`. The `cursor` provider MUST take its endpoints, timers, and client version from `senpi`'s `packages/ai/src/api/cursor-agent.ts`.

The run request MUST carry the conversation id, the model id as requested and displayed model, and the conversation state. The root prompt list MUST start with the system prompt blob `{"role":"system","content":...}`, where an empty system prompt becomes `You are a helpful assistant.`. Earlier messages MUST follow as JSON blobs in the donor format: user text, assistant text with `tool-call` parts, and `tool` messages with `tool-result` parts. A blob id MUST be the SHA-256 of its bytes. When the last message is user text, that text MUST be the user message action. Otherwise the action MUST be resume. A stored checkpoint whose first root prompt equals the current system blob MUST supply the other state fields. Its root prompts and turns MUST be replaced.

The provider MUST answer each blob read from the request blobs, then from blobs the server stored. The provider MUST answer a missing blob without data. The provider MUST store a blob write before the reply. The provider MUST store each checkpoint. The provider MUST answer a request-context exec with the request tools as MCP definitions. Each definition MUST carry the tool schema as a `google.protobuf.Value` without `oneOf`, `anyOf`, or `allOf` members. The provider MUST NOT advertise the tool named `exec`, because the shell exec frame runs it.

The `cursor` provider MUST build its requests as `senpi`'s `buildGrpcRequest` and `buildMcpToolDefinitions` do in `packages/ai/src/api/cursor-agent.ts`. The `cursor` provider MUST encode `google.protobuf.Value` with the field numbers that the `google.protobuf` reference defines at `https://protobuf.dev/reference/protobuf/google.protobuf/#value`.

Shell, shell-stream, and MCP exec frames MUST run through the `InBand` callback exactly once. A shell frame MUST map to the tool `exec` with `command` and, when present, `cwd` and `timeout_ms`. The `timeout_ms` value MUST come from `hard_timeout`, which is the kill deadline in milliseconds. The `timeout` field is the wait in milliseconds before Cursor moves a command to the background, and it MUST NOT become a kill deadline. An MCP frame MUST map to its tool name with its arguments decoded from `google.protobuf.Value`. The idle timer MUST pause while the callback runs, and the client MUST send an exec heartbeat every three seconds. The reply MUST carry the tool output text. A tool error MUST set exit code 1 or the MCP error flag. After the reply, the provider MUST emit `ToolCall{Resolved:true}`. An MCP frame marked approval-only MUST NOT run, and the provider MUST answer it as rejected. Without a callback, the provider MUST reply that in-band tool execution is unavailable and MUST NOT emit a tool call. The provider MUST answer an unknown exec frame with an exec throw. Every exec frame MUST end with an exec stream close.

The provider MUST read server frames while an in-band call runs. An exec-server abort for the running exec MUST cancel the callback context. The provider MUST then send no reply, MUST send the exec stream close, and MUST emit `ToolCall{Resolved:true}`. An abort for a queued exec MUST skip that exec and MUST still send its stream close. The provider MUST drop interaction heartbeats during a call. The provider MUST copy the other frames that arrive during a call and handle them in order after the call. At most 64 frames MAY wait. When 64 frames wait, the provider MUST cancel the callback context and MUST fail the attempt. The provider MUST send each exec stream close before it closes the request. It MUST stop waiting when the server sends the turn-ended update or when the response ends. It MUST also stop 3 seconds after it sends that close. A canceled turn MUST end the wait at once.

The provider MUST answer each interaction query with an interaction response that carries the query id and the matching result field. The provider MUST answer web search, mode switch, Exa search, Exa fetch, and web fetch queries as rejected, with a reason. The provider MUST answer an ask-question query with a rejected result. The provider MUST answer a create-plan query with a plan error. A VM setup query and an unknown query MUST fail the attempt, because their results cannot express a refusal.

Text and reasoning updates MUST produce text and reasoning deltas. The turn-ended update MUST produce usage and `Done{stop}` and MUST end the attempt. Turn input usage MUST exclude cache reads and writes. An error trailer MUST fail with the HTTP status of its Connect code. A stream that ends before turn-ended MUST fail with status 503. An attempt that started an in-band call MUST NOT retry. `Models` MUST call `GetUsableModels` and MUST skip empty and duplicate ids.

Cursor conversation state MUST live in the caller's `StateDir`, one directory per thread. The directory MUST hold `conversation-id`, `checkpoint`, and `blobs/<hex id>`. The provider MUST create a missing conversation id as a random UUID. The provider MUST write each file to a temporary file, rename it, and give it mode 0600. An empty `StateDir` MUST use a new conversation id per stream and keep state in memory only.

## Streams

The stream MUST report text deltas, reasoning deltas, complete tool calls, usage, a stop reason, or an error. Tool arguments MUST preserve raw JSON when the provider supplies JSON. A decoder MUST NOT emit partial tool calls. Interleaved calls MUST retain separate ids, names, and argument buffers. Invalid complete JSON arguments MUST fail the stream.

OpenAI reasoning summaries and reasoning text MUST produce reasoning deltas. Anthropic `thinking_delta` blocks MUST produce reasoning deltas. Chat-compatible decoders MUST accept `reasoning_content`, `reasoning`, and `reasoning_text`, in that precedence order. A decoder MUST skip a reasoning field that is not a string and use the next field. Google parts with `thought:true` MUST produce reasoning deltas. An Anthropic `redacted_thinking` block MUST keep its `data` string on every tool call of that response. When thinking is enabled, the encoder MUST replay each one unchanged as `{"type":"redacted_thinking","data":...}` after the thinking block and before text and `tool_use`.

Usage MUST separate uncached input, cached reads, and cache writes. OpenAI and chat-compatible usage MUST subtract cache reads and writes from total prompt tokens. Chat-compatible cache reads MUST prefer `prompt_tokens_details.cached_tokens`, then `prompt_cache_hit_tokens`, then top-level `cached_tokens`, in any key order. Anthropic usage MUST merge non-null fields from `message_start` and `message_delta`. A null field MUST keep the earlier value. Google usage MUST subtract cached content from input and add thought tokens to output.

Stop reasons MUST use `stop`, `length`, or `toolUse`. Provider safety, refusal, unknown, and terminal error reasons MUST fail the stream. OpenAI refusal events and Google `promptFeedback.blockReason` MUST fail the stream. A stream without its required terminal event MUST fail. A stream that emitted a tool call, text, or reasoning MUST NOT retry, because the retry would re-emit the partial content after the failed attempt. A stream failure MUST retain the raw event that caused it.

Google SSE data MAY contain one response object or an array of response objects. Only the first candidate MUST produce events.

When Google returns a function call without an id, the decoder MUST generate an id with a decoder-unique prefix. When Google returns an id that the decoder already saw, the decoder MUST generate an id with a decoder-unique prefix. A first-seen provider-supplied id MUST stay unchanged.

The stream MUST stop when its context is canceled. A consumer MAY use `Next` or range over `Events`. The event queue MUST remain bounded. `Emit` after `Finish` MUST return false and MUST NOT send. An SSE read that ends on cancel, start timeout, or idle timeout MUST close the body. The read MUST wait for its reader goroutine before it returns. When the reader had already reached a clean end of stream, the read MUST return success.

## Capabilities and roles

`Models` MUST use the provider list endpoint and MUST follow its pagination until the provider reports no further page. Anthropic MUST page with `after_id` set to `last_id` while `has_more` is true. Google MUST page with `nextPageToken`. A repeated page cursor MUST fail. Each capability record MUST include the model id, context window, output limit, effort levels, and tool-choice support when known. Unknown numeric limits MUST remain zero, except for the context default. The context window MUST use the configured value for the bare model id first, then the provider-reported input limit, then 128000. Anthropic MUST read `max_input_tokens`, `max_tokens`, and the levels under `capabilities.effort`. Chat-compatible lists MUST read `context_length`, then `top_provider.context_length`, and `top_provider.max_completion_tokens`. Google MUST read `inputTokenLimit` and `outputTokenLimit` and skip models without `generateContent`. Other list endpoints MUST leave effort levels nil.

The Anthropic model list decoder MUST read the fields of the List Models API at `https://platform.claude.com/docs/en/api/models/list`.

The chat-compatible model list MUST decode the response shape of `pi`'s `packages/ai/src/providers/openrouter-api.ts`.

The Anthropic, OpenAI, and Google adapters MUST report images for each listed model. Every other adapter MUST report images for a model only when its provider metadata says that the model accepts images. A chat-compatible model MUST report images only when its list says that its input modalities include `image`. The Anthropic, OpenAI, and Google adapters MUST still report image support when catalog requests fail. They MUST also report it when the selected model is absent.

Static role presets MUST match the preset table in [model-presets.md](../decisions/model-presets.md). Providers without a static preset MUST use their first offered model for `smol` and `slow`. An unavailable preset MUST return the thread model and this exact log line: `roles.<name> preset <id> is not offered; using <model>`.

Providers MUST take their static preset identifiers from the native model guides of OpenAI, Anthropic, and Google, as cited in `docs/dal/decisions/model-presets.md`, dated 2026-11-08.

An explicit reasoning effort MUST match a level of the model's `efforts` capability before any network call. A declared level that the model does not offer MUST fail the request of a top-level turn. The error MUST be `model <model_id> does not support reasoning effort <requested>; supported: [<levels>]`. A thread that is not top-level, such as an advisor or a subagent, MUST NOT fail for that reason. It MUST send the highest declared level below the requested level. It MUST send no effort level when no declared level is below the requested level. The level order is `minimal`, `low`, `medium`, `high`, `xhigh`, `max`. A declared level outside that order is never a fallback. A model that declares no effort level leaves the capability unknown. An unknown capability MUST send the request without an effort level. An empty effort or `default` MUST send no effort level. The `effort` field of a turn MUST win over a keyword effort. A keyword effort `max` MUST apply only when the model declares `max`. An unsupported keyword effort MUST be ignored.

A provider that does not implement image input MUST refuse a turn that carries an image. The server MUST refuse with `this model does not accept images`. The server MUST replace the images of earlier turns with a text notice for such a model.

## Cost

The server MUST price each `assistant.message` that carries usage. It MUST use the model of the thread at that message. That model starts as `session.created.model` and then follows each `user.message.model`.

The price MUST come from `[models."<id>"]` in [config.md](config.md) first. Without that table, the price MUST come from the built-in table in [model-prices.md](../decisions/model-prices.md). A model with neither has no price.

The cost of one message is the sum of its four token counts, each times its rate in micro-USD per million tokens. The sum MUST divide by 1000000 and round half up once. The result MUST saturate at the largest `int64`.

The thread cost is the sum of its message costs. A message on a model with no price adds no cost. Such a message MUST set `costPartial` when another message has a price. [protocol.md](protocol.md) defines the wire members.

The cost covers the assistant messages of the thread. It MUST NOT include advisor, decider, compaction, or subagent spend. The journal MUST NOT record a cost. The server computes the cost on read, so a new price table prices old threads again.

## Retry and timeouts

Transport failures, 408, 429, and 5xx responses MUST be retryable. An HTTP 200 body or stream error with a numeric 429 or 5xx code MUST be retryable unless it names a quota code. Context-length errors MUST return `Overflow`. 401 and 403 MUST return `Auth`. Usage-limit and other errors MUST be terminal. A 429 whose body names a quota code as a JSON string MUST be terminal. The quota codes are `insufficient_quota`, `credit_balance_exhausted`, `organization_spend_limit_exceeded`, `project_spend_limit_exceeded`, `organization_usage_limit_exceeded`, `usage_limit_reached`, and `usage_not_included`. A 402 MUST be terminal. Transient phrases such as `timeout` MUST be matched only against local transport error text, never against a provider body. Model text in a refusal therefore cannot cause a retry.

A request MUST retry at most three times. Delays MUST be one, two, and four seconds with plus or minus twenty percent jitter. A `Retry-After` value is whole seconds or an HTTP date. A value that does not parse or is not positive MUST be ignored. A valid value up to 60 seconds MUST replace the delay. With a client attached, a value above 60 seconds and up to 300 seconds MUST wait, and the wait MUST be reported through `dalgona/retry/wait`. A larger value MUST stop the request with `rate limited for <s> s, over the <limit> s wait limit`. In that error, `<limit>` is 300 with a client attached and 60 without one. `<s>` is the wait rounded up to whole seconds. The error MUST keep the HTTP status and body. The retry callback MUST run before sleeping.

A client that shows a rate-limit wait MUST show `rate limited · retrying in <m>m <ss>s · esc stops` for the remaining time. `<m>` is a whole number of minutes, and `<ss>` is the remaining seconds in two digits. Esc MUST send `turn/interrupt`, which ends the wait. A client that honours reduced motion MUST show the countdown as static text.

The first-event deadline MUST cover connection, response headers, and the first SSE event. It MUST stop after the first event. The idle deadline MUST measure received bytes. An idle value of zero MUST disable the idle deadline. The idle controller MUST support pause and resume for in-band tool execution. Timeout failures MUST be retryable. Errors MUST retain HTTP status and raw provider error bodies when available.
