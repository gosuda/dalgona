# Shell argument parser

## Invariant

`internal/dal/shellargs` MUST hold the only command splitter, token reader, and token quoter of the module. Another package MUST NOT keep a copy, a wrapper, or an alias of them. The package MUST import only the Go standard library. Core packages MAY import it. Interface and battery packages MUST NOT import it. `internal/dal/runner` MUST NOT import `internal/dal/agent/rules` to reach the parser.

## Scope

This decision applies to `internal/dal/shellargs`, its importers, and the depguard `core` rule in `.golangci.yml`. That rule lists `**/internal/shellargs/**`. The importers are `internal/dal/agent` and `internal/dal/agent/rules`. `internal/dal/runner` MAY import the package for its process-kill guard.

`Split` cuts a command into parts at the characters `;`, `|`, `&`, `(`, `)`, `{`, `}` and the newline. An operator inside quotes or after a backslash stays in its part. `Split` trims each part and drops empty parts. A part such as `''` has text, so `Split` keeps it.

Single quotes and double quotes group text. A backslash escapes the next character outside quotes and inside double quotes. A backslash inside single quotes is plain text.

`Tokens` reads one part into words. It removes quotes and backslash escapes and keeps operator characters as text. It returns false for an unterminated quote and for a trailing backslash.

A pair of quotes with nothing inside, such as `''` or `""`, makes one empty word. `Tokens` MUST keep that word at its place, so `a '' b` gives three words. Unquoted whitespace alone makes no word. Empty quotes that touch other text add no word.

`QuoteToken` returns a token unchanged when the token is not empty and holds no whitespace, quote, or backslash. Otherwise it wraps the token in single quotes and writes each single quote as `'\''`.

[Approvals](../spec/approvals.md) states how the approval state uses the parts of `Split`.

## Enforcement

`just lint` runs the depguard `core` rule. The rule denies an import of the interface and battery packages from `internal/dal/shellargs`. `TestSplitProperty` in `internal/dal/shellargs/shellargs_test.go` checks that `Split` returns the generated parts for quotes, escapes, and every operator.

The tests `TestGrantPrefixRejectsUnterminatedQuote`, `TestGrantPrefixRejectsTrailingBackslash`, `TestGrantPrefixAcceptsWellFormedPrefix`, `TestGrantPrefixKeepsFlagSecondToken`, and `TestGrants` in `internal/dal/agent/rules` check `Tokens` and `QuoteToken` through `GrantPrefix` and `MatchesGrant`. The test `TestDenyRuleEmptyQuotedWordKeepsItsPlace` checks the empty word through `Check`.

`TestTokens` and `TestQuoteToken` check both functions by hand-worked cases, including empty quoted words. `TestQuoteTokenRoundTripsThroughTokens` checks that `Tokens` returns each quoted token unchanged. Its generated tokens include the empty token. No lint rule denies an import of another core package from `internal/dal/shellargs`. The `clients` rule does not list `internal/dal/shellargs`. Review checks both import limits.

## Rejected alternatives

- A copy of the parser in each consumer. It lost because the copies can split one command differently. A rule check and a runner guard then disagree about that command.
- The parser stays in `internal/dal/agent/rules` and the runner imports `internal/dal/agent/rules`. It lost because the runner would depend on a package of the agent tree, and the agent tree already imports the runner.
