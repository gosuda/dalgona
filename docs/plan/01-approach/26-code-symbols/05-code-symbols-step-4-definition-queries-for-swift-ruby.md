#### Code symbols step 4. Definition queries for Swift, Ruby, PHP, Bash, Lua and Markdown

Anchor: Herbert Simon

- Files: new queries `swift.scm`, `ruby.scm`, `php.scm`, `bash.scm`, `lua.scm` and `markdown.scm`. `internal/syntax/language.go`: six registry rows. The grammar packages are `grammars/swift`, `grammars/ruby`, `grammars/php`, `grammars/bash`, `grammars/lua` and `grammars/markdown`. The extension rows are `.swift` for swift, `.rb` for ruby, `.php` for php, `.sh` for bash, `.lua` for lua, and `.md` for markdown (decision 30). Fixtures under `internal/syntax/testdata/<name>/`. `internal/syntax/syntax_test.go`: six fixture tests. `THIRD_PARTY_NOTICES.md`: six sections, for swift, ruby, php, bash, lua and markdown.
- Queries, exactly.
  - `swift.scm`:

```
(protocol_declaration (type_identifier) @name) @definition.interface
(class_declaration "class" (type_identifier) @name (class_body)) @definition.class
(class_declaration "actor" (type_identifier) @name (class_body)) @definition.class
(class_declaration "struct" (type_identifier) @name (class_body)) @definition.type
(class_declaration (type_identifier) @name (enum_class_body)) @definition.enum
(protocol_function_declaration (simple_identifier) @name) @definition.method
(class_body (function_declaration (simple_identifier) @name) @definition.method)
(source_file (function_declaration (simple_identifier) @name) @definition.function)
(enum_entry (simple_identifier) @name) @definition.constant
(typealias_declaration name: (type_identifier) @name) @definition.type
```

  - `ruby.scm`:

```
(class name: [(constant) (scope_resolution)] @name) @definition.class
(module name: [(constant) (scope_resolution)] @name) @definition.module
(program (method name: (_) @name) @definition.function)
(program (singleton_method name: (_) @name) @definition.function)
(body_statement (method name: (_) @name) @definition.method)
(body_statement (singleton_method name: (_) @name) @definition.method)
(assignment left: (constant) @name) @definition.constant
```

  - `php.scm`:

```
(function_definition name: (name) @name) @definition.function
(method_declaration name: (name) @name) @definition.method
(class_declaration name: (name) @name) @definition.class
(interface_declaration name: (name) @name) @definition.interface
(trait_declaration name: (name) @name) @definition.interface
(enum_declaration name: (name) @name) @definition.enum
(enum_case name: (name) @name) @definition.constant
(const_declaration (const_element . (name) @name)) @definition.constant
```

  - `bash.scm`:

```
(function_definition name: (word) @name) @definition.function
```

  - `lua.scm`:

```
(function_declaration name: (identifier) @name) @definition.function
(function_declaration name: (dot_index_expression field: (identifier) @name)) @definition.method
(function_declaration name: (method_index_expression method: (identifier) @name)) @definition.method
```

  - `markdown.scm`:

```
(section (atx_heading heading_content: (inline) @name)) @definition.section
(section (setext_heading heading_content: (paragraph) @name)) @definition.section
```

Basis: the library's override rows at `grammars/tags_query_infer.go:308-314` (ruby), `:237-245` (php), `:279` (bash), `:658-661` (lua) and `:343-352` (swift). The library has no markdown row. The Ruby rows use the `name` field, because the library's rows match a method's object in `def obj.m` too (node types at the pinned ref of ruby, `licenses/grammars.json`). The Ruby split is by parent: a top-level `def` is `function`, and a `def` in a class or module body is `method` (decision 31). A `def` nested in a method body is dropped by decision 6. The Lua colon form is a separate row, because the library's row covers only dot names. The Markdown rows give each heading's `section` node a span, so a definition reads its body (decision 36). A section ends where a heading of the same or higher level starts. If a Markdown row does not match its fixture, delete that row and its fixture lines, and the kind table then names the gap.
- Fixtures and expected rows, written before the queries. Each row is `kind name start end depth`.
  - `testdata/swift/definitions.swift`, lines: 1 `protocol Named {`; 2 `    func name() -> String`; 3 `}`; 4 `class Box {`; 5 `    func open() {}`; 6 `}`; 7 `struct Pt {`; 8 `    var x: Int`; 9 `}`; 10 `enum Mode {`; 11 `    case fast`; 12 `}`; 13 `typealias Id = Int`. Expected: `interface Named 1 3 0`; `method name 2 2 1`; `class Box 4 6 0`; `method open 5 5 1`; `type Pt 7 9 0`; `enum Mode 10 12 0`; `constant fast 11 11 1`; `type Id 13 13 0`.
  - `testdata/ruby/definitions.rb`, lines: 1 `module Tools`; 2 `  LIMIT = 8`; 3 `  class Runner`; 4 `    def run`; 5 `    end`; 6 `  end`; 7 `  def self.build`; 8 `  end`; 9 `end`; 10 `def top`; 11 `end`; 12 `def obj.m`; 13 `end`; 14 `class Store::Vault`; 15 `  def name=(v)`; 16 `  end`; 17 `end`. Expected: `module Tools 1 9 0`; `constant LIMIT 2 2 1`; `class Runner 3 6 1`; `method run 4 5 2`; `method build 7 8 1`; `function top 10 11 0`; `function m 12 13 0`; `class Store::Vault 14 17 0`; `method name= 15 16 1`.
  - `testdata/php/definitions.php`, lines: 1 `<?php`; 2 `function helper() {}`; 3 `class Box {`; 4 `    public function open() {}`; 5 `}`; 6 `interface Named {}`; 7 `trait Logs {}`; 8 `enum Mode {`; 9 `    case Fast;`; 10 `}`; 11 `const LIMIT = 8;`. Expected: `function helper 2 2 0`; `class Box 3 5 0`; `method open 4 4 1`; `interface Named 6 6 0`; `interface Logs 7 7 0`; `enum Mode 8 10 0`; `constant Fast 9 9 1`; `constant LIMIT 11 11 0`.
  - `testdata/bash/definitions.sh`, lines: 1 `greet() {`; 2 `  echo hi`; 3 `}`; 4 `function run_all {`; 5 `  greet`; 6 `}`. Expected: `function greet 1 3 0`; `function run_all 4 6 0`.
  - `testdata/lua/definitions.lua`, lines: 1 `local function helper()`; 2 `  return 1`; 3 `end`; 4 `function Box.open(self)`; 5 `  return self`; 6 `end`; 7 `function Box:close(self)`; 8 `  return self`; 9 `end`; 10 `function run()`; 11 `end`. Expected: `function helper 1 3 0`; `method open 4 6 0`; `method close 7 9 0`; `function run 10 11 0`.
  - `testdata/markdown/definitions.md`, lines: 1 `# Title`; 2 blank; 3 `Intro text.`; 4 blank; 5 `## Setup`; 6 blank; 7 `Setup text.`; 8 blank; 9 `Heading text`; 10 `---`. Expected: `section Title 1 10 0`; `section Setup 5 8 1`; `section Heading text 9 10 1`.
- Tests that fail before the change: `TestSymbolsFixtureSwift`, `TestSymbolsFixtureRuby`, `TestSymbolsFixturePHP`, `TestSymbolsFixtureBash`, `TestSymbolsFixtureLua` and `TestSymbolsFixtureMarkdown`, all in `internal/syntax/syntax_test.go`.
- Specification text: none in this step.
- Lands after: Code symbols step 3. Definition queries for Java, C, C++, C# and Kotlin

