# Ask tool

## Invariant

`ask` is a read-class core tool for top-level threads of both products. It reaches the user only through `item/tool/requestUserInput`. It adds no journal event. A wait ends on an answer, a dismissal, an interrupt, the last subscriber leaving, or the 5-minute timeout. A tool that waits for a person runs only as a direct call: no cell, workflow or plugin calls ask.

## Scope

The package `internal/dal/tool/ask` holds the tool, its argument parser, its cleaning rules, and its result text. This decision permits the imports `internal/dal/agent` to `internal/dal/tool/ask`, `internal/dal/rpc` to `internal/dal/tool/ask`, and `internal/dal/script` to `internal/dal/tool/ask`. The package itself imports only `internal/dal/tool` and `internal/dal/model`.

The `core` depguard rule is a deny list, not an allow list for these imports.

## Enforcement

The `core` depguard rule in `.golangci.yml` keeps `internal/dal/tool/ask` free of interface and battery packages. `internal/dal/tool/ask/ask_test.go` checks the argument limits, the cleaning rules, the answer normalisation, and the result texts against `docs/dal/spec/ask.md`.

`TestAskToolOnlyOnTopLevelThreads` and `TestAskToolInDal` in `internal/dal/agent/ask_tool_test.go` check the top-level tool scope. `TestScriptCannotCallAsk` in `internal/dal/script/eval_test.go` checks the direct-call rule.

`TestQuestionTimesOutExactlyAtTimeout` in `internal/dal/agent/question_timeout_test.go` checks the timeout length. `TestSandboxChoiceTimeoutMakesNoChoice` in the same file and `TestAnswerFromReplyTimedOut` in `internal/dalgona/battery/ratchet_internal_test.go` check that a consent question takes no default.

## Rejected alternatives

- HTML forms, as oh-my-pi offers. A form control gives the model a second input path that a terminal cannot show, so the tool uses options, previews, and one typed answer.
- Password fields. A question never asks for a secret, because a secret would then enter the transcript.
- A configurable timeout length. The tool keeps one fixed 5-minute timeout, and the timed-out result names the recommended options, so a stuck wait cannot hide behind a longer setting.
- A timeout that restarts on a key press. A restart needs a wire message that reports activity, and the owner asked for one constant.
- Skipping the timeout. Without it a window that stays attached and that nobody reads would hold the turn forever.
- A default for a consent question. The first option of a ratchet approval is `Approve`, so a default would approve a digest that no person read. Only the `ask` tool applies the recommended option.
- An extension method instead of the Codex method `item/tool/requestUserInput`. The vendored schema already defines the request, and both clients already read it.
