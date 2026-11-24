# Text input package

## Invariant

`internal/dal/textinput` MUST hold the only Go editing segmenter, Unicode property tables, and generic input-field checks of the module.
It also holds the escape recognition profile of [Ask tool](../spec/ask.md#escape-recognition-profile), the nonblank eligibility check, and the safe display projections of user input text.
`web/src/lib/text` MUST hold the only TypeScript copy of the segmenter, tables, and field checks.
Another package MUST NOT keep a second editing segmenter, field check, or projection of user input text, or a wrapper or alias of one.
Terminal cell measurement and the TUI sanitizer for server, model, and tool output stay under [TUI](../spec/tui.md).
Both packages implement [Input text](../spec/input-text.md) at Unicode 18.0.0 and UAX #29 revision 49.

One generator writes both table files from the same pinned Unicode Character Database files.
The generator MUST refuse a source file whose SHA-256 differs from its pin.
The tables MUST NOT be edited by hand.

The Go package MUST import only the Go standard library.
Core, server, lifecycle, and client packages MAY import it.
`internal/dal/tui` MAY import it beside `internal/dal/rpc`.
`internal/dal/rpc` MUST NOT import it. The wire package rejects malformed encoding with the standard library alone.

## Scope

This decision applies to `internal/dal/textinput`, `web/src/lib/text`, their importers, and the depguard rules in `.golangci.yml`.
The `core` rule lists `**/internal/dal/textinput/**`.
The `tui-imports-only-rpc` rule allows `internal/dal/textinput` beside `internal/dal/rpc`.
The `text-input-leaf` rule denies every module-internal import from the package.

`go generate ./internal/dal/textinput` runs `gen.go`.
It downloads `GraphemeBreakProperty.txt`, `emoji-data.txt`, `DerivedCoreProperties.txt`, and `PropList.txt` from `https://www.unicode.org/Public/18.0.0/ucd/`.
It writes `tables.go` and `web/src/lib/text/tables.ts`.
`pnpm -C web exec biome format --write src/lib/text/tables.ts` then gives the committed TypeScript layout.
The tables pack `Grapheme_Cluster_Break`, `Extended_Pictographic`, `Indic_Conjunct_Break`, `Default_Ignorable_Code_Point`, and `White_Space`.

`testdata/GraphemeBreakTest-18.0.0.txt` is the upstream conformance file at its pinned SHA-256.
The [shared corpus](../spec/input-text-cases.json) supplies the product vectors.
The Unicode License V3 notice for the data files is in `THIRD_PARTY_NOTICES.md`.

## Enforcement

`just lint` runs the depguard rules. They deny interface imports from the package, every module-internal import from it, and every other internal import from the TUI.
`TestBoundariesPassUnicodeConformance` and `TestStepsFollowUnicodeConformanceBothWays` check every upstream conformance line in Go.
`web/src/lib/text/text.test.ts` checks the same lines and the same pinned hash in TypeScript.
`TestCorpusVectorsKeepTheirCountsAndBoundaries` and its TypeScript peer check the shared corpus in both languages.
`TestEscapeEndFollowsTheRecognitionProfile` and `TestEligibleIgnoresOnlyInvisibleScalarsAndCompleteEscapes` check the recognition rows of the ask acceptance table.
A table change without a matching generator run fails one of those tests or shows in review as a hand edit.

## Rejected alternatives

- `github.com/rivo/uniseg`. It lost because its release implements an older Unicode version, and the contract pins 18.0.0.
- `Intl.Segmenter` in the web client. It lost because each browser ships its own Unicode version, and the contract forbids trusting an advertised runtime version.
- Separate generators for Go and TypeScript. It lost because two generators can read different data and split one text differently.
