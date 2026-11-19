#### Code symbols step 1. Syntax package boundary and Go definitions

Anchor: David Parnas

- Files: new `internal/syntax/language.go`, `internal/syntax/symbols.go`, `internal/syntax/queries/go.scm`, `internal/syntax/syntax_test.go`, `internal/syntax/testdata/go/definitions.go`, `docs/decisions/syntax-package.md`. `THIRD_PARTY_NOTICES.md`: create it with the heading `# Third-party notices` when it is absent, and append to it when another workstream created it first. `go.mod` and `go.sum`: run `go get github.com/odvcencio/gotreesitter@v0.57.0`, then `go mod tidy`. `.golangci.yml`: under `depguard`, `rules`, add the rules `syntax` and `copyleft` (below) at the indent of `fast`, and add `"**/internal/syntax/**"` to the `files` list of the `core` rule after `"**/internal/decide/**"` (line 108). `docs/decisions/dependencies.md`: replace the Decision cell of the gotreesitter row that the Version pins workstream wrote with `Syntax library for the symbols, definition, syntax and rewrite modes, see docs/decisions/syntax-package.md`. `internal/tool/search` is not touched in this step.
- Measure before any edit: on linux/amd64, run `go build -o bin/dal ./cmd/dal` and `go build -o bin/dalgona ./cmd/dalgona`, and record both sizes in `docs/decisions/syntax-package.md`.
- Library facts used here. Library paths are relative to the module root `github.com/odvcencio/gotreesitter@v0.57.0`. `grammars/go/go.go:19` declares `func Language() *gotreesitter.Language`. `parser_api.go:67` declares `type TokenSourceFactory func(source []byte) (TokenSource, error)`. `grammars/runtime/token_source_factory_registry.go:25` declares `func TokenSourceFactory(name string) func(src []byte, lang *gotreesitter.Language) gotreesitter.TokenSource`, and that file's package clause is `grammarruntime`. `parser_api.go:1236` declares `SetTimeoutMicros(timeoutMicros uint64)`, `:1271` declares `SetMemoryBudgetBytes(bytes int64)`, and `:1299` declares `SetParseWorkLimits(limits ParseWorkLimits)`. `parser_api.go:1123-1129` declares `ParseWorkLimits` with the `int` fields `IterationLimit`, `StackDepthLimit` and `NodeLimit`. `parser_api.go:1829` declares `ParseStrict(source []byte) (*Tree, error)`, and `:2053` declares `ParseWithTokenSourceFactoryStrict(source []byte, factory TokenSourceFactory) (*Tree, error)`. `parser_api.go:2775` declares `ErrParseStoppedEarly`, and `:37-38` declares `ParseStoppedEarlyError.Is`. `tree.go:14` declares `Range`, with `StartPoint` at `:17` and `EndPoint` at `:18`. `tree.go:2256` declares `Node.HasError`. `tree.go:4149-4150` declares `Tree.Release` and its nil check. `outline.go:152` declares `NewOutliner(lang *Language, tagsQuery string, opts ...OutlinerOption) (*Outliner, error)`, and `outline.go:239` declares `OutlineTree(tree *Tree) ([]OutlineSymbol, OutlineReport)`. `outline_report.go:9` declares `OutlineSymbol` with `Kind string` (`:14`), `Name string` (`:28`), `Range Range` (`:33`) and `Children []OutlineSymbol` (`:47`). `outline_report.go:86` declares `OutlineReport`, with `DeclineReason string` (`:142`) and `Truncated bool` (`:145`).
- Change, `language.go`: `type language struct { name string; extensions []string; grammar func() *gotreesitter.Language; query string; outliner func() (*gotreesitter.Outliner, error) }`. `newLanguage(name string, extensions []string, grammar func() *gotreesitter.Language, query string) *language` sets `outliner` with `sync.OnceValues` over `gotreesitter.NewOutliner(grammar(), query)`. The method `func (l *language) syntaxOnly() bool` returns `l.query == ""`. The method `func (l Language) HasDefinitions() bool` returns `!l.l.syntaxOnly()`, and `symbolsMode` uses it to count syntax-only files. The registry is `var registry = []*language{newLanguage("go", []string{".go"}, golang.Language, goQuery)}`, with `//go:embed queries/go.scm` on `var goQuery string`. The grammar import is `golang "github.com/odvcencio/gotreesitter/grammars/go"`. `ForPath(path string) (Language, bool)` lowercases `filepath.Ext(path)` and looks for it in the extensions with a loop. `ByName(name string) (Language, bool)`. `Names() []string` returns the registered names sorted. `type Language struct { l *language }` with `Name() string` and `NewParser() *Parser`. `NewParser` returns `newParser(l.l, parseTimeout)`, with `parseTimeout = 2 * time.Second`. The constants are `parseMemoryBudgetBytes int64 = 256 << 20`, `parseIterationLimit = 200_000`, `parseStackDepthLimit = 8_192` and `parseNodeLimit = 1_000_000`.
- Change, adapter and parser setup: `type Parser struct { lang *language; grammar *gotreesitter.Language; parser *gotreesitter.Parser; timeout time.Duration }`. `newParser(lang *language, timeout time.Duration) *Parser` calls `grammar := lang.grammar()`, then `gotreesitter.NewParser(grammar)`, then `SetTimeoutMicros(uint64(timeout / time.Microsecond))`, then `SetMemoryBudgetBytes(parseMemoryBudgetBytes)`, then `SetParseWorkLimits(gotreesitter.ParseWorkLimits{IterationLimit: parseIterationLimit, StackDepthLimit: parseStackDepthLimit, NodeLimit: parseNodeLimit})`. The adapter is this, with the import `grammarruntime "github.com/odvcencio/gotreesitter/grammars/runtime"`.

```go
func tokenSourceFactory(name string, lang *gotreesitter.Language) gotreesitter.TokenSourceFactory {
	factory := grammarruntime.TokenSourceFactory(name)
	if factory == nil {
		return nil
	}
	return func(src []byte) (gotreesitter.TokenSource, error) {
		return factory(src, lang), nil
	}
}
```

- Change, `symbols.go`: `type Kind string`. Unexported constants `kindFunction` through `kindSection` hold the eleven closed kinds, with the values `function`, `method`, `class`, `interface`, `type`, `enum`, `constant`, `variable`, `module`, `macro` and `section`. `func kindOf(library string) (Kind, error)` is the one mapping table. It maps the eleven kinds to themselves, `constructor` to `kindMethod`, and `record` to `kindClass`. Any other input returns `fmt.Errorf("syntax: unknown kind %s", library)`. `type Symbol struct { Kind Kind; Name string; Start, End, Depth int }`. `type Report struct { SyntaxErrors, Stopped bool }`.
- Change, `parse`: `func (p *Parser) parse(ctx context.Context, src []byte) (*gotreesitter.Tree, Report, error)`. If `ctx.Err()` is not nil, return it wrapped as `syntax: %w`. Record `began := time.Now()`. When `tokenSourceFactory(p.lang.name, p.grammar)` is non-nil, call `ParseWithTokenSourceFactoryStrict(src, f)`. Otherwise call `ParseStrict(src)`. When `errors.Is(err, gotreesitter.ErrParseStoppedEarly)` or `time.Since(began) >= p.timeout`, release any returned tree and return `nil, Report{Stopped: true}, nil`. Any other error returns `fmt.Errorf("syntax: parse %s: %w", p.lang.name, err)`. Otherwise return the tree and `Report{SyntaxErrors: tree.RootNode().HasError()}`.
- Change, `Symbols`: `func (p *Parser) Symbols(ctx context.Context, src []byte) ([]Symbol, Report, error)`. When `p.lang.syntaxOnly()` is true, return `syntax: no definition query for NAME`. Call `p.lang.outliner()`, and wrap any error as `syntax: outliner NAME: %w`. Call `parse`, and when the tree is nil return the report and the error. Then `defer tree.Release()`, call `OutlineTree(tree)`, and read the report. A non-empty `DeclineReason` returns `syntax: outline declined: REASON`. A true `Truncated` returns `nil, Report{Stopped: true}, nil`. Otherwise flatten the forest. Walk it depth first, in order. Map the kind of every symbol with `kindOf`, so an unknown kind anywhere is an error. `Start` is `Range.StartPoint.Row + 1` and `End` is `Range.EndPoint.Row + 1`. A child is kept only when its parent's kind is `class`, `interface`, `type`, `module`, `enum` or `section`. A dropped child takes its subtree with it. `Depth` is the number of kept ancestors. The output is in pre-order, so it is sorted by start line.
- Query `internal/syntax/queries/go.scm`, exactly:

```
(function_declaration name: (identifier) @name) @definition.function
(method_declaration name: (field_identifier) @name) @definition.method
(method_elem name: (field_identifier) @name) @definition.method
(type_spec name: (type_identifier) @name type: (interface_type)) @definition.interface
(type_spec name: (type_identifier) @name type: [(struct_type) (type_identifier) (qualified_type) (generic_type) (pointer_type) (array_type) (slice_type) (map_type) (channel_type) (function_type) (parenthesized_type)]) @definition.type
(type_alias name: (type_identifier) @name) @definition.type
(const_spec . name: (identifier) @name) @definition.constant
(var_spec . name: (identifier) @name) @definition.variable
```

The first six rows follow the library's Go override (`grammars/tags_query_infer.go:119-140`). The library's struct row at line 133 is merged into the type row. The `method_elem` row is moved up. The library comment at `:129-132` says it does not capture package-level `var` and `const`, because one spec can bind several names. The last two rows anchor on the first name, with `.`, so that a spec binds only its first name. The Go fixture expects both rows. If the fixture does not match them, delete both rows and their fixture lines, and the kind table then names the gap.
- Depguard rules, under `rules:` at the indent of `fast`:

```
        syntax:
          files:
            - "!**/internal/syntax/**"
          deny:
            - pkg: "github.com/odvcencio/gotreesitter"
              desc: only internal/syntax imports the tree-sitter library
        copyleft:
          files:
            - $all
          deny:
            - pkg: "github.com/odvcencio/gotreesitter/grammars$"
              desc: the aggregate package embeds the copyleft grammars; import grammars/<name>
            - pkg: "github.com/odvcencio/gotreesitter/grammars/(caddy|disassembly|jq|nim)$"
              desc: copyleft grammar under GPL-3.0 or MPL-2.0 per licenses/grammars.json
```

- Tests that fail before the change, all in `internal/syntax/syntax_test.go`:
  - `TestForPathExtensionTable`. Inputs and expected values: `main.go` gives `go` and true; `MAIN.GO` gives `go` and true; `main.txt` gives false; `ByName("go")` gives true; `ByName("nope")` gives false; `Names()` gives `[go]`. Source: the registry row of this step.
  - `TestKindTable`. Inputs: the thirteen library kinds `function`, `method`, `class`, `interface`, `type`, `enum`, `constant`, `variable`, `module`, `macro`, `section`, `constructor` and `record`. Expected: each closed kind maps to itself, `constructor` maps to `method`, and `record` maps to `class`. Source: decision 5 and the table of this step.
  - `TestKindRejectsOutsideSet`. Input: `struct`. Expected error text: `syntax: unknown kind struct`. Source: decision 5.
  - `TestSymbolsGoFixture`. Input: `testdata/go/definitions.go`, the 26 lines below. Expected rows, as kind, name, start, end and depth: `type Shape 3 5 0`; `interface Named 7 9 0`; `method Name 8 8 1`; `type Count 11 11 0`; `type Alias 13 13 0`; `constant Limit 15 15 0`; `variable Default 17 17 0`; `function Area 19 22 0`; `method Double 24 26 0`. The expected rows are a table literal at the top of the test. Source: the Go forms of this step, written before the query.
  - `TestSymbolsLocalDefinitionsOmitted`. Input: `func F() {\n\tx := 1\n\t_ = x\n}\n`. Expected: one `function F 1 4 0` and no child. Source: decision 6.
  - `TestSymbolsRejectsContextCancelled`. Input: a cancelled context and the source `package p`. Expected: the error wraps `context.Canceled`. Source: decision 21.
  - `TestSymbolsStoppedWithTimeout`. Input: a Go file of at least 1 MiB, made of the line `package p` and then lines `func fNNNNNN() int { return NNNNNN }`, where each six-digit NNNNNN is drawn from `rand.New(rand.NewPCG(1, 2))`. The test calls `newParser(registry[0], time.Microsecond).Symbols`. Expected: `Report.Stopped` is true and there are no symbols. Source: decision 9. The stop comes from the library timeout, so the test does not wait on a clock.
- Fixture `testdata/go/definitions.go`, 26 lines, exactly: 1 `package sample`; 2 blank; 3 `type Shape struct {`; 4 `\tW int`; 5 `}`; 6 blank; 7 `type Named interface {`; 8 `\tName() string`; 9 `}`; 10 blank; 11 `type Count int`; 12 blank; 13 `type Alias = Shape`; 14 blank; 15 `const Limit = 8`; 16 blank; 17 `var Default = Shape{}`; 18 blank; 19 `func Area(s Shape) int {`; 20 `\ttotal := s.W`; 21 `\treturn total`; 22 `}`; 23 blank; 24 `func (s Shape) Double() Shape {`; 25 `\treturn s`; 26 `}`.
- Mutation probes, both reverted after the run. Add `internal/tool/search/probe_syntax.go`, which imports `github.com/odvcencio/gotreesitter`. Run `just lint`, and it must report the `syntax` rule. Add `internal/syntax/probe_copyleft.go`, which imports `github.com/odvcencio/gotreesitter/grammars/caddy`. Run `just lint`, and it must report the `copyleft` rule. Delete both files. Source: the rule text above. The probes also check the `!` negation in `files` and the regex form of `pkg`, which the repository's rules use (`encoding/json$`).
- Notices: `THIRD_PARTY_NOTICES.md` gets two sections. `## gotreesitter (MIT)` holds the copyright line `Copyright (c) 2026 Oscar Villavicencio` (`LICENSE:3`) and the MIT permission text of that LICENSE file. `## go grammar (MIT)` holds the copyright line `Copyright (c) 2014 Max Brunsfeld` and the MIT permission text at the URL for `go` in the Schemas table.
- Specification text: none in this step, because it adds no user-visible behaviour.
- Lands after: Edit-review step 1. Line diff package; Code search step 3. Index builder, publish rules and hidden build command.

