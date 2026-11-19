#### Code symbols step 5. Syntax-only grammars for Svelte, HTML, CSS, JSON, YAML and TOML

Anchor: Dieter Rams

- Files: `internal/syntax/language.go`: six registry rows with an empty query, which makes them syntax only. The grammar packages are `grammars/svelte`, `grammars/html`, `grammars/css`, `grammars/json`, `grammars/yaml` and `grammars/toml`. The extension rows are `.svelte` for svelte, `.html` and `.htm` for html, `.css` for css, `.json` for json, `.yaml` and `.yml` for yaml, and `.toml` for toml. `internal/syntax/testdata/<name>/sample.<ext>`: one file each. `internal/syntax/syntax_test.go`: two tests, listed below. `THIRD_PARTY_NOTICES.md`: six sections, for svelte, html, css, json, yaml and toml.
- Sample files. `sample.svelte`: `<script>`, `  let n = 1;`, `</script>`, `<p>{n}</p>`. `sample.html`: `<!doctype html>`, `<p>hi</p>`. `sample.css`: `p { color: red; }`. `sample.json`: `{"a": [1, 2]}`. `sample.yaml`: `a: 1`, `b:`, `  - x`. `sample.toml`: `a = 1`, `[t]`, `b = "x"`.
- Tests that fail before the change, all in `internal/syntax/syntax_test.go`:
  - `TestSyntaxOnlyGrammarsParse`. Input: each sample file. The test calls `ParseStrict` on a parser from `NewParser`. Expected: the error is nil, and `RootNode().HasError()` is false. Source: each grammar parses its own sample without a syntax error.
  - `TestSyntaxOnlySymbolsRefused`. Input: the svelte sample through `Symbols`. Expected error text: `syntax: no definition query for svelte`. Source: decision 19.
  - `TestNamesListsAllLanguages`. Expected: `Names()` returns the 23 names of the Schemas section, sorted.
  - `TestForPathEveryExtension`. Table: every extension row of the Schemas extension table, with its language name, for example `.h` gives `c`, `.sh` gives `bash`, `.md` gives `markdown`, and `.htm` gives `html`. Expected: each row gives its language and true. Source: the Schemas extension table.
- Specification text: none in this step. The syntax mode in Step 8 uses these languages.
- Lands after: Code symbols step 4. Definition queries for Swift, Ruby, PHP, Bash, Lua and Markdown

