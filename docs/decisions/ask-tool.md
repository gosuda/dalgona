# Ask tool

## Invariant

`ask` is a read-class core tool for top-level threads of both products. It reaches the user only through `item/tool/requestUserInput`. It adds no journal event. It has no timeout: a wait ends on an answer, a dismiss, an interrupt, or the last window leaving.

## Scope

The package `internal/tool/ask` holds the tool, its argument parser, its cleaning rules, and its result text. This decision permits the imports `internal/agent` to `internal/tool/ask`, `internal/rpc` to `internal/tool/ask`, and `internal/script` to `internal/tool/ask`. The package itself imports only `internal/tool` and `internal/model`.

## Enforcement

The `core` depguard rule in `.golangci.yml` keeps `internal/tool/ask` free of interface and battery packages. `internal/tool/ask/ask_test.go` checks the argument limits, the cleaning rules, the answer normalisation, and the result texts against `docs/spec/ask.md`.

## Rejected alternatives

- HTML forms, as oh-my-pi offers. A form control gives the model a second input path that a terminal cannot show, so the tool uses options, previews, and one typed answer.
- Password fields. A question never asks for a secret, because a secret would then enter the transcript.
- A timeout that returns a timed-out text. The user may take minutes to answer, and a timer would make the model choose for the user.
- An extension method instead of the Codex method `item/tool/requestUserInput`. The vendored schema already defines the request, and both clients already read it.
