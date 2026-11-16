# Decision clients

## Client contract

`Client.Ask` MUST accept a JSON state and a map of named questions. It MUST return a `Result` that holds `Answers`, `Model` and `Fallback`. It MUST NOT change the state or the questions.

The state MUST be a JSON string, object, or array. Other JSON values MUST fail before a network request starts.

A question MUST have one of these types:

| Type | Criteria | Direct LLM value | Decisions answer field |
| --- | --- | --- | --- |
| `choice` | REQUIRED object of option keys and guidance | Selected option key string | `choice` |
| `noul` | OPTIONAL object with `true` and `false` guidance | Probability of yes as a number | `noul` |
| `score` | REQUIRED non-empty array of ordered guidance | Expected criteria index from 0 to `len(criteria)-1` | `score` |

An unsupported question type MUST fail. Missing required criteria MUST fail. Choice and noul criteria MUST be objects when present. Score criteria MUST be a non-empty array. Invalid criteria MUST fail before a network request starts.

`Answer.Noul` and `Answer.Score` MUST be nil when the answer does not have that value. `Answer.Probabilities` and `Answer.Confidence` MUST preserve the values returned by a Decisions model. An LLM adapter MUST NOT invent these values. It MUST return nil probabilities and zero confidence.

## Adapter specifications

`Parse` MUST accept `decisions:<id>` and `llm:<role or id>`. It MUST return the adapter name and the complete model or role identifier. It MUST reject an unknown adapter, a missing colon, or an empty identifier with `decide: spec must be decisions:<id> or llm:<role or id>`.

## Decisions adapter

The Decisions adapter MUST send `POST https://openrouter.ai/api/alpha/decisions`. A configured base URL MAY replace this endpoint in tests or deployments.

The request MUST have `Content-Type: application/json`, `Accept: application/json`, and `Authorization: Bearer <token>`. The token callback MUST receive the request context. A token callback failure MUST stop the request.

The JSON body MUST contain `model`, `state`, and `questions`. Question names MUST have a deterministic order. Criteria and answers MUST use the shapes in the [Decisions API schema](https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-request.md). The [Jev guide](https://openrouter.ai/docs/guides/community/jev) defines the typed question semantics.

The adapter MUST decode the response as a stream. It MUST read the top-level `answers` object and ignore unknown response fields. Every requested answer MUST exist. Its `type` MUST match its question type. Its required value MUST exist. A refusal, type mismatch, missing answer, malformed number, or malformed JSON MUST fail.

A refusal is an answer whose `type` is `refusal`.

Probabilities MUST decode as an object of numeric values. Choice and score answers MAY include confidence and probabilities. Noul answers use their `noul` number directly.

A non-2xx response MUST fail with its HTTP status. The error body MUST be limited to 1 MiB.

## LLM adapter

The LLM adapter MUST use the supplied `model.Provider` and model identifier. It MUST place the state and questions in one user message. It MUST set a system prompt that requires only one JSON object and no other text. The prompt MUST define the score scale as the expected zero-based index into the criteria.

The response object MUST map each question name to its direct value. It MUST contain every requested name and no other name. Duplicate names MUST fail. Choice values MUST be strings. Noul and score values MUST be numbers. A score outside `0` through `len(criteria)-1` MUST fail. The adapter MUST NOT accept Markdown fences, prose, explanations, tool calls, and separate answer objects.

The adapter does not check that a noul value lies in 0 through 1.

The adapter MUST concatenate text deltas in stream order. A provider stream failure MUST fail the request. Invalid JSON or an invalid answer shape MUST return an error containing `llm: invalid JSON`.

## Time limits

Every `Ask` call MUST use a child context with an 8-second default timeout. `DecisionsConfig.Timeout` and `NewLLMWithTimeout` MAY set a shorter timeout for tests or deployments. A non-positive configured timeout MUST select the default.

A shorter caller deadline MUST take precedence. Cancellation and deadline errors MUST remain available through `errors.Is`.

## Fallback

`WithFallback` wraps one primary client and one smol client. It returns the primary unchanged when no smol client is supplied. It also returns the primary unchanged when both canonical model identities are known and equal.

Each call returns `context.Cause(ctx)` first when the parent context is already done. It checks parent cancellation before each attempt and before accepting a result.

The primary attempt runs under a fresh 8-second deadline. Lazy client construction and `Ask` both run inside that attempt. The attempt context is canceled when the attempt finishes.

An expired attempt is normalized to its context cause. A typed HTTP error keeps its status and its retry hint through that normalization.

A successful primary result returns its answering model and an empty `Fallback` string. A parent cancellation or deadline returns the parent cause without fallback.

A primary availability, credential, provider resolution, HTTP, transport or timeout failure records a cooldown of `max(60 s, Retry-After)`. `ErrAnswer` and `ErrRequest` record no cooldown. Parent cancellation records no cooldown.

The cooldown map is host owned, mutex protected and memory only. It is keyed by the primary model identity. An update keeps the later expiry. Expired entries are removed on lookup and insertion.

A cooldown hit supplies a `*CooldownError` as the primary cause and skips the primary request. The skip does not extend the expiry.

The smol attempt gets a separate fresh 8-second deadline, bounded by the caller context. Its success returns the smol answering model. `Result.Fallback` holds `<primary model>: <primary error>`.

A failed smol attempt returns a `*FallbackError` that stores both original typed causes. Its `Unwrap` returns both causes without replacing them with strings.

`Result.Model` names the answering model, never the requested role. An empty answering model or a missing answer fails as an answer category error.

A refusal, tool-call answer, malformed JSON, missing answer, type mismatch or invalid score reports `ErrAnswer`. A request encoding failure or an invalid question definition reports `ErrRequest`. Both categories trigger fallback.

`decide.HTTPError` and the provider `model.HTTPError` carry the status and the parsed `Retry-After`. Callers read them with `errors.As`. No caller parses error text to recover a status or classify a failure.
