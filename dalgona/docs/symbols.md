# Symbols: search and structural editing

dal and dalgona provide syntax-aware symbol search and structural block editing over supported programming languages.
Symbol intelligence relies on Tree-sitter parsers to extract semantic definitions and syntactic blocks directly from source code.
No language server protocol daemon runs; symbol operations are bounded, deterministic parse passes over current file bytes.

## Configuration

Symbol capabilities are gated by the `search_symbols` setting in `dal.toml`.
In dal, `search_symbols` is `false` by default.
In dalgona, `search_symbols` is `true` by default.

- `search: mode "symbol" is off. Set search_symbols = true in dal.toml.`
- `patch: line <n>: block ops (N*) need search_symbols = true in dal.toml.`

## Definition kinds

Symbol extraction folds language-specific syntax nodes onto a closed set of twelve definition kinds:

| Kind | Code construct | Typical scope |
|---|---|---|
| `function` | Free function or procedure | Module or top-level |
| `method` | Method inside a class, impl block, trait, or receiver | Type or instance scope |
| `struct` | Struct or union definition | Type definition |
| `enum` | Enumeration or variant type | Type definition |
| `trait` | Trait definition | Interface definition |
| `impl` | Implementation block named by target type | Type implementation |
| `module` | Module, namespace, or package | Namespace definition |
| `macro` | Macro definition | Macro definition |
| `const` | Constant, immutable value, or static definition | Value definition |
| `type` | Type alias or type definition | Type definition |
| `class` | Class definition | Object type |
| `interface` | Interface or module type definition | Interface contract |

Supported languages include Rust, TypeScript, JavaScript, Go, Python, OCaml, C, and C++.
Files lacking parser extractors report:
- `search: <p> has no symbol support (<lang>).`
- `patch: <p> has no symbol support (<lang>).`

## Symbol search

The `search` tool accepts `mode: "symbol"` with a `pattern` string.
The query pipeline matches definitions from fresh parses of current file bytes.
Trigram literals from the final segment filter candidate files, followed by fresh syntax walks.

### Pattern grammar

```
pattern = "*" / [ "::" ] segment *( "::" segment )
segment = name [ "[" ordinal "]" ]
ordinal = 1*digit
```

- Suffix matching: `pattern` matches any definition whose qualified name ends with the specified segments. For example, `run` matches `Engine::run` and `Worker::run`.
- Absolute matching: A leading `::` pins the match to the complete qualified name. For example, `::Engine::run` matches only top-level `Engine::run`.
- Ordinal disambiguation: `[k]` specifies the 1-based source-order definition among identical names in the same file. For example, `apply[2]` targets the second definition named `apply`.
- File outline: `pattern: "*"` with `path` set to one file lists every definition in that file in source order.

Invalid patterns return specific errors:
- `search: invalid symbol pattern <pattern>: <reason>.`
  Valid reason strings are `empty`, `empty segment`, `ordinal must be a number`, `ordinal must be at least 1`, or `* with more than one path`.
- `search: pattern * needs path set to one file.`
- `search: <p> is not a file.`
- `search: parser failed on <p>: <msg>.`

### Budgets and caps

- Maximum candidate files parsed: 500 files per query.
- Each file parse has a 250 ms wall-clock budget. A query can parse multiple files.
- Single match reveal limit: at most 2000 lines and 50 KiB (51,200 bytes).
- Text files up to 16 MiB are parsed; binary and UTF-16 files are skipped.

### Output forms

Multiple matches list matching rows followed by a guidance footer:
```
lib/patch.ml:40-88 function apply
lib/patch.ml:120-130 function apply[2]
lib/tool.ml:12-30 function Tool::apply
[3 definitions. Search a longer name to see one whole with its tag.]
```

A single match within limits reveals the entire definition with line numbers and def tag:
```
lib/patch.ml:40-88 function apply tag 7C2E4B1A
40:let apply w edits =
...
88:  Ok changes
```

Outline queries print definitions without the guidance footer:
```
lib/patch.ml:40-88 function apply
lib/patch.ml:120-130 function apply[2]
[2 definitions.]
```

When search limits are reached:
- `[<n> definitions shown; parse budget reached.]`
- `No definitions match <pattern> in <scope>.`

## Structural block editing

Structural editing enables edits anchored to definition boundaries or syntax blocks.

### Patch by symbol

The `patch` tool accepts `symbol` change objects:
- `symbol`: Definition name matching search mode symbol output (e.g., `Engine::run` or `run[2]`).
- `tag`: Optional 8-character hexadecimal tag (`tag8("def:" ++ definition_raw_bytes)`). When supplied, patch verifies that the definition has not changed since it was read.
- `at`: Insertion position relative to definition: `"replace"` (default), `"before"`, or `"after"`.
- `new`: Replacement or inserted text. Passing `new: ""` with `at: "replace"` deletes the definition.
- `old`: When replacing text within a definition without replacing the whole block, provide `symbol`, `old`, and `new`. `old` must occur exactly once inside the definition; no `tag` is needed.

Symbol patch errors:
- `patch: changes[<i>]: tag <T> does not match <name> now (tag <U>). The file changed since you read it; read it again.`
- `patch: changes[<i>]: definition tag needs a symbol selector.`
- `patch: no definition <name> in <p>.`
- `patch: <n> definitions named <name> in <p>.`
- `patch: changes[<i>]: old is not in <name> in <p>. Copy it again from the lines below, or read <p>.`
- `patch: <p> has no symbol support (<lang>).`
- `patch: changes[<i>]: symbol support is not enabled for this file.`
- `patch: parser failed on <p>: <reason>.` The reason is one of `Budget`, `TooManyParses`, `Panic`, or `Unsupported`.

### Hashline N* block operator

In the `hashline` edit style, the `N*` operator references the outermost named syntactic block beginning at line `N`:
- `PUT N*:` replaces the lines of the resolved syntactic block.
- `CUT N*` deletes the lines of the resolved syntactic block.
- `PUT >N*:` inserts text after the last line of the syntactic block at sibling indentation.

Block syntax errors:
- `patch: no syntactic block begins at line <n> of <p>.`
- `patch: line <n>: block ops (N*) need search_symbols = true in dal.toml.`
