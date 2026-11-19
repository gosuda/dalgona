#### Schemas of the Code symbols workstream

The `search` tool after Step 8. The `mode` property takes one of five values: `find`, `grep`, `symbols`, `definition` and `syntax`. `pattern` is a string property that is not in `required`, so the list `required` holds only `mode`. The code enforces `pattern` for `find`, `grep`, `definition` and `syntax`. The property `kind` is an array of strings, and each string is one of the eleven kinds. The property `lang` is a string, and it takes one of the 23 names. `additionalProperties` stays false. `limit` keeps its range of 1 to 1000 and its default of 200. `path` keeps its default of `.`.

Modes and arguments:

| Mode | pattern | kind | lang | limit counts |
|---|---|---|---|---|
| find | required | not allowed | not allowed | paths |
| grep | required | not allowed | not allowed | matches |
| symbols | optional | allowed | not allowed | symbol rows |
| definition | required, not empty | allowed | not allowed | body rows |
| syntax | required, a query | not allowed | required | capture rows |

In the `definition` mode, `kind` filters the definitions whose headers print. In the `symbols` mode, a child row keeps its depth indent under a kind filter.

The closed kind set and its uses:

| Kind | Used for |
|---|---|
| function | Go functions; TypeScript, TSX and JavaScript top-level functions, generator functions, and `const` bindings of arrow functions or function expressions; Python top-level and decorated functions; Rust top-level functions and functions in `mod` bodies; C function definitions, up to two pointer levels; C++ free functions; Kotlin top-level functions; Swift top-level functions; PHP functions; Bash functions; Lua `function name` and `local function`; Ruby top-level `def` and `def obj.m` |
| method | Go methods and interface method elements; TypeScript, TSX and JavaScript class methods, private names included; Python methods in a class body; Java and C# methods and constructors; C++ methods in a class body and out-of-class definitions; Kotlin and Swift member functions and Swift protocol methods; Rust functions in `impl` and `trait` bodies, listed at depth 0; PHP methods; Ruby `def` and `def self.x` in a class or module body; Lua dot and colon functions |
| class | Java, C# and Kotlin classes with a body; Java and C# records (through the refinement to `record`); Kotlin objects; C++ classes; TypeScript classes and abstract classes; Python classes; Ruby classes; Swift classes and actors; PHP classes |
| interface | Go interfaces; TypeScript interfaces; Java interfaces and annotation types; C# interfaces; Kotlin interfaces; Swift protocols; Rust traits; PHP interfaces and traits |
| type | Go type declarations and aliases; TypeScript type aliases; C typedefs, structs and unions; C++ typedefs, structs and unions; C# structs; Rust structs, unions and type aliases; Swift structs and typealiases; Kotlin type aliases |
| enum | Enums in TypeScript, Java, C#, Rust, PHP, Swift, C and C++; Kotlin enum classes |
| constant | Go constants; Rust `const` items; Ruby constant assignments; PHP enum cases and class constants; Kotlin enum entries; Swift enum cases |
| variable | Go package-level variables; Rust `static` items |
| module | Ruby modules; TypeScript namespaces; Rust `mod` items; C++ namespaces |
| macro | Reserved. No query in this workstream emits it. |
| section | Markdown headings, one `section` node for each heading |

Not listed: Kotlin classes without a body, Kotlin and Swift properties, Java enum constants, TypeScript dotted namespaces, Rust macros, C prototypes, and Go package-level names after the first in a spec.

The extension table after Step 5. A shared extension belongs to one language only. `.h` belongs to `c`.

| Extension | Language |
|---|---|
| .go | go |
| .ts, .mts, .cts | typescript |
| .tsx | tsx |
| .js, .jsx, .mjs, .cjs | javascript |
| .py | python |
| .rs | rust |
| .java | java |
| .c, .h | c |
| .cc, .cpp, .cxx, .hpp, .hh, .hxx | cpp |
| .cs | c_sharp |
| .kt, .kts | kotlin |
| .swift | swift |
| .rb | ruby |
| .php | php |
| .sh | bash |
| .lua | lua |
| .md | markdown |
| .svelte | svelte |
| .html, .htm | html |
| .css | css |
| .json | json |
| .yaml, .yml | yaml |
| .toml | toml |

The 23 languages, their grammar packages and whether they have a definition query:

| Name | Grammar package | Definition query |
|---|---|---|
| bash | grammars/bash | yes |
| c | grammars/c | yes |
| c_sharp | grammars/c_sharp | yes |
| cpp | grammars/cpp | yes |
| css | grammars/css | no, syntax only |
| go | grammars/go, package golang | yes |
| html | grammars/html | no, syntax only |
| java | grammars/java | yes |
| javascript | grammars/javascript | yes |
| json | grammars/json | no, syntax only |
| kotlin | grammars/kotlin | yes |
| lua | grammars/lua | yes |
| markdown | grammars/markdown | yes |
| php | grammars/php | yes |
| python | grammars/python | yes |
| ruby | grammars/ruby | yes |
| rust | grammars/rust | yes |
| svelte | grammars/svelte | no, syntax only |
| swift | grammars/swift | yes |
| toml | grammars/toml | no, syntax only |
| tsx | grammars/tsx | yes |
| typescript | grammars/typescript | yes |
| yaml | grammars/yaml | no, syntax only |

The package API after Step 8. The Code symbols steps export the names below. `Language`, `Parser` and `Query` are exported types with unexported fields. Code rewrite step 1 adds `Report.ErrorLine`, `(*Query).CaptureNames` and `Language.Extensions` to this surface, and no other name:

```
func ForPath(path string) (Language, bool)
func ByName(name string) (Language, bool)
func Names() []string
func (l Language) Name() string
func (l Language) NewParser() *Parser
func (l Language) HasDefinitions() bool
func (l Language) Compile(pattern string) (*Query, error)
func (p *Parser) Symbols(ctx context.Context, src []byte) ([]Symbol, Report, error)
func (p *Parser) Captures(ctx context.Context, q *Query, src []byte) ([]Capture, Report, error)
type Kind string
type Symbol struct { Kind Kind; Name string; Start, End, Depth int }
type Capture struct { Name string; Line, Column, StartByte, EndByte, Match int }
type Report struct { SyntaxErrors, Stopped bool }
```

The error texts, exactly as the code returns them:

- `search: mode must be find, grep, symbols, definition, or syntax`
- `search: pattern must not be empty` (find, grep, definition and syntax)
- `search: unknown kind NAME`
- `search: kind applies only to symbols and definition`
- `search: lang is required in syntax mode`
- `search: unknown lang NAME, use one of: LIST`, where LIST holds the 23 names in sorted order, separated by commas
- `search: lang applies only to syntax mode`
- `search: name patterns cannot use ^, $, \A, \z, \b or \B; match the name only`
- `search: parse deadline reached`
- `syntax: ` followed by the library message for a query that does not compile
- `syntax: parse NAME: ` followed by the wrapped error for any other parse failure
- `syntax: context canceled` (for a cancelled context, through `%w`)
- `syntax: unknown kind KIND`
- `syntax: outline declined: REASON`
- `syntax: no definition query for NAME`
- `syntax: outliner NAME: ` followed by the wrapped error from `NewOutliner`

The footers, exactly as the code prints them, in this order:

- `[Parse: stopped, no rows: PATH]`, one line for each file
- `[Parse: syntax errors, rows kept: PATH]`, one line for each file
- `[Not parsed: N files. Narrow path or glob.]`, for files over the file cap, beyond the file count cap, or beyond the byte cap
- `[Syntax only: N files. Use mode syntax with lang.]`
- `[Stopped: N files not parsed. Narrow path or glob.]`, for candidates after the call budget
- `[Truncated: N more symbols]`, `[Truncated: N more lines]` or `[Truncated: N more captures]`, always the last line

The empty replies: `No matches for PATTERN. Searched N files of supported languages.` for symbols and definition, `No symbols. Searched N files of supported languages.` for symbols with no pattern, and `No matches for PATTERN. Searched N files of language NAME.` for syntax.

The row forms: `PATH:START-END: KIND TEXT` for a symbol or a definition header, with two spaces before the path for each level of depth. `PATH:LINE: TEXT` for a definition body row. `PATH:LINE: CAPTURE: TEXT` for a syntax row.

The constants:

| Constant | Value | Rule |
|---|---|---|
| parseTimeout | 2 seconds | per parse, through `SetTimeoutMicros` (`parser_api.go:1236`) |
| parseMemoryBudgetBytes | 256 MiB | per parse, through `SetMemoryBudgetBytes` (`parser_api.go:1271`) |
| parseIterationLimit | 200,000 | work limit (`parser_api.go:1125`) |
| parseStackDepthLimit | 8,192 | work limit (`parser_api.go:1127`) |
| parseNodeLimit | 1,000,000 | work limit (`parser_api.go:1129`) |
| parseMaxFiles | 256 | per call |
| parseMaxFileBytes | 256 KiB | per file |
| parseMaxBytes | 1 MiB | per call |
| parseBudget | 10 seconds | per call, soft stop from the start of the child |
| parseDeadline | 30 seconds | per child, hard kill from `Tool.Run` |

The rule that holds the constants: parseDeadline is at least parseBudget plus parseTimeout plus 10 seconds. The measured values that set the bounds are in decisions 9 and 10.

The notices of the 23 grammars, from the entries of `licenses/grammars.json` (all MIT, checked by the owner on 2026-11-12). Each section holds its copyright lines and the MIT permission text of the licence file at the URL. Step 1 adds `gotreesitter` and `go`. Step 2 adds the five rows of its language group, and so on.

| Name | Copyright lines | Licence file |
|---|---|---|
| bash | Copyright (c) 2017 Max Brunsfeld | https://github.com/tree-sitter/tree-sitter-bash/blob/a06c2e4415e9bc0346c6b86d401879ffb44058f7/LICENSE |
| c | Copyright (c) 2014 Max Brunsfeld | https://github.com/tree-sitter/tree-sitter-c/blob/b780e47fc780ddc8da13afa35a3f4ed5c157823d/LICENSE |
| c_sharp | Copyright (c) 2014-2023 Max Brunsfeld, Damien Guard, Amaan Qureshi, and contributors. | https://github.com/tree-sitter/tree-sitter-c-sharp/blob/9150f7d56bb47f1a809fa23623f1ba1413e93fa9/LICENSE |
| cpp | Copyright (c) 2014 Max Brunsfeld | https://github.com/tree-sitter/tree-sitter-cpp/blob/c009222808634c1014f82438d4883753516a2c24/LICENSE |
| css | Copyright (c) 2018 Max Brunsfeld | https://github.com/tree-sitter/tree-sitter-css/blob/dda5cfc5722c429eaba1c910ca32c2c0c5bb1a3f/LICENSE |
| go | Copyright (c) 2014 Max Brunsfeld | https://github.com/tree-sitter/tree-sitter-go/blob/2346a3ab1bb3857b48b29d779a1ef9799a248cd7/LICENSE |
| html | Copyright (c) 2014 Max Brunsfeld | https://github.com/tree-sitter/tree-sitter-html/blob/73a3947324f6efddf9e17c0ea58d454843590cc0/LICENSE |
| java | Copyright (c) 2017 Ayman Nadeem | https://github.com/tree-sitter/tree-sitter-java/blob/e10607b45ff745f5f876bfa3e94fbcc6b44bdc11/LICENSE |
| javascript | Copyright (c) 2014 Max Brunsfeld | https://github.com/tree-sitter/tree-sitter-javascript/blob/58404d8cf191d69f2674a8fd507bd5776f46cb11/LICENSE |
| json | Copyright (c) 2014 Max Brunsfeld | https://github.com/tree-sitter/tree-sitter-json/blob/254c42a6476413b776221e03982ac8ae159eeb72/LICENSE |
| kotlin | Copyright (c) 2019 fwcd | https://github.com/fwcd/tree-sitter-kotlin/blob/1852ea17b7f60fb3f9d84e0b1555d56b46b39fb1/LICENSE |
| lua | Copyright (c) 2021 Munif Tanjim | https://github.com/tree-sitter-grammars/tree-sitter-lua/blob/10fe0054734eec83049514ea2e718b2a56acd0c9/LICENSE.md |
| markdown | Copyright (c) 2021 Matthias Deiml | https://github.com/tree-sitter-grammars/tree-sitter-markdown/blob/a0a00f817d02412bd92c54d316f164d827b57b5c/LICENSE |
| php | Copyright (c) 2017 Josh Vera, GitHub; Copyright (c) 2019 Max Brunsfeld, Amaan Qureshi, Christian Frøystad, Caleb White | https://github.com/tree-sitter/tree-sitter-php/blob/3fda2fb9577166c6399834917f9844f30370beea/LICENSE |
| python | Copyright (c) 2016 Max Brunsfeld | https://github.com/tree-sitter/tree-sitter-python/blob/26855eabccb19c6abf499fbc5b8dc7cc9ab8bc64/LICENSE |
| ruby | Copyright (c) 2016 Rob Rix | https://github.com/tree-sitter/tree-sitter-ruby/blob/ad907a69da0c8a4f7a943a7fe012712208da6dee/LICENSE |
| rust | Copyright (c) 2017 Maxim Sokolov | https://github.com/tree-sitter/tree-sitter-rust/blob/77a3747266f4d621d0757825e6b11edcbf991ca5/LICENSE |
| svelte | Copyright (c) 2024 Amaan Qureshi <amaanq12@gmail.com> | https://github.com/tree-sitter-grammars/tree-sitter-svelte/blob/ae5199db47757f785e43a14b332118a5474de1a2/LICENSE |
| swift | Copyright (c) 2021 alex-pinkus | https://github.com/alex-pinkus/tree-sitter-swift/blob/00bbb0a2550f8bc0023a2a4992922d51ae045626/LICENSE |
| toml | Copyright (c) Ika <ikatyang@gmail.com> (https://github.com/ikatyang) | https://github.com/tree-sitter/tree-sitter-toml/blob/342d9be207c2dba869b9967124c679b5e6fd0ebe/LICENSE |
| tsx | Copyright (c) 2017 Max Brunsfeld | https://github.com/tree-sitter/tree-sitter-typescript/blob/75b3874edb2dc714fb1fd77a32013d0f8699989f/LICENSE |
| typescript | Copyright (c) 2017 Max Brunsfeld | https://github.com/tree-sitter/tree-sitter-typescript/blob/75b3874edb2dc714fb1fd77a32013d0f8699989f/LICENSE |
| yaml | Copyright (c) 2024 tree-sitter-grammars contributors; Copyright (c) 2019-2021 Ika | https://github.com/tree-sitter-grammars/tree-sitter-yaml/blob/a1c4812a73ec5e089de8e441fdea3a921e8d5079/LICENSE |

