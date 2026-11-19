#### Spec-fix step 4. Always send `model`, and fail an empty model before the request
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/decide.md` line 31 says: "The JSON body MUST contain `model`, `state`, and `questions`." Keep that sentence.

- Where: `internal/decide/json.go:writeAsk`, `internal/decide/decisions.go:Decisions.Ask`.
- Change: In writeAsk (json.go:40-42), write the model name token without the `if modelID != ""` guard. In Decisions.Ask (decisions.go:46), before encodeAsk at line 49, return the error `decisions: model identifier is required` when d.model is empty, and send no request. Side: code, because the OpenRouter request schema requires model, so an omitted key breaks the protocol.
- Failing-first test: `TestDecisionsAskRejectsEmptyModel` in `internal/decide/decisions_test.go`. Input: NewDecisions(DecisionsConfig{BaseURL: httptest server URL that counts requests, Model: "", Token: returns "t"}); Ask(ctx, state `"x"`, questions {"is_bug": {Type: TypeNoul}}) Expected: Ask returns the error text `decisions: model identifier is required`, and the server counts 0 requests; today the request is sent without a model key, so the test fails first The expected value comes from: docs/spec/decide.md:31 gives the rule; the OpenRouter DecisionsRequest schema lists required model, state, questions (submit-a-decisions-request.md:427-430); the error text is chosen by this plan; decisions_test.go:150-152 shows the model is sent when set
- Evidence at commit `3c4b6f94`: json.go:40-41 `if modelID != "" {` omits the key when empty; decisions.go:49 `encodeAsk(d.model, state, questions)` has no empty check; no test pins the omission; decide.go:51 Parse already rejects an empty identifier
- Proof: the test fails before the change and passes after it, and `just verify` passes.

