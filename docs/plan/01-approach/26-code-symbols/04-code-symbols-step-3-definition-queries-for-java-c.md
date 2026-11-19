#### Code symbols step 3. Definition queries for Java, C, C++, C# and Kotlin

Anchor: Christopher Alexander, Herbert Simon

- Files: new `internal/syntax/queries/java.scm`, `c.scm`, `cpp.scm`, `c_sharp.scm` and `kotlin.scm`. `internal/syntax/language.go`: five registry rows. The grammar packages are `grammars/java`, `grammars/c`, `grammars/cpp`, `grammars/c_sharp` (import alias `csharp`) and `grammars/kotlin`. The extension rows are `.java` for java, `.c` and `.h` for c, `.cc`, `.cpp`, `.cxx`, `.hpp`, `.hh` and `.hxx` for cpp, `.cs` for c_sharp, and `.kt` and `.kts` for kotlin. The `.h` extension belongs to `c` only (decision 20). The factories for c, cpp and java already exist in the runtime package, and `Parser.parse` (Step 1) uses them with no new branch. Fixtures under `internal/syntax/testdata/<name>/`. `internal/syntax/syntax_test.go`: five fixture tests and three hostile-input tests. `THIRD_PARTY_NOTICES.md`: five sections, for java, c, cpp, c_sharp and kotlin.
- Queries, exactly.
  - `java.scm`:

```
(class_declaration name: (identifier) @name) @definition.class
(interface_declaration name: (identifier) @name) @definition.interface
(enum_declaration (identifier) @name) @definition.enum
(record_declaration (identifier) @name) @definition.class
(annotation_type_declaration (identifier) @name) @definition.interface
(method_declaration name: (identifier) @name) @definition.method
(constructor_declaration name: (identifier) @name) @definition.method
```

  - `c.scm`:

```
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @definition.function
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @definition.function
(function_definition declarator: (pointer_declarator declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name)))) @definition.function
(type_definition declarator: (type_identifier) @name) @definition.type
(struct_specifier name: (type_identifier) @name body: (_)) @definition.type
(union_specifier name: (type_identifier) @name body: (_)) @definition.type
(enum_specifier name: (type_identifier) @name body: (_)) @definition.enum
```

  - `cpp.scm`:

```
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @definition.function
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @definition.function
(function_definition declarator: (function_declarator declarator: (qualified_identifier name: (identifier) @name))) @definition.method
(field_declaration_list (function_definition declarator: (function_declarator declarator: (field_identifier) @name)) @definition.method)
(type_definition declarator: (type_identifier) @name) @definition.type
(struct_specifier name: (type_identifier) @name body: (_)) @definition.type
(union_specifier name: (type_identifier) @name body: (_)) @definition.type
(class_specifier name: (type_identifier) @name body: (_)) @definition.class
(enum_specifier name: (type_identifier) @name body: (_)) @definition.enum
(namespace_definition name: (namespace_identifier) @name) @definition.module
```

  - `c_sharp.scm`:

```
(method_declaration name: (identifier) @name) @definition.method
(constructor_declaration (identifier) @name) @definition.method
(class_declaration name: (identifier) @name) @definition.class
(interface_declaration name: (identifier) @name) @definition.interface
(enum_declaration name: (identifier) @name) @definition.enum
(struct_declaration (identifier) @name) @definition.type
(record_declaration (identifier) @name) @definition.class
```

  - `kotlin.scm`:

```
(class_declaration "interface" (type_identifier) @name) @definition.interface
(class_declaration (type_identifier) @name (enum_class_body)) @definition.enum
(class_declaration "class" (type_identifier) @name (class_body)) @definition.class
(object_declaration (type_identifier) @name) @definition.class
(source_file (function_declaration (simple_identifier) @name) @definition.function)
(class_body (function_declaration (simple_identifier) @name) @definition.method)
(enum_entry (simple_identifier) @name) @definition.constant
(type_alias (type_identifier) @name) @definition.type
```

Basis: the library rows at `grammars/tags_query_infer.go:175-182` (java), `:194-201` (c), `:202-210` (cpp), `:211-217` (c_sharp) and `:327-335` (kotlin). The library emits `constructor` for constructors (`:180`, `:216`), which the table of decision 5 maps to `method`. The library's C function row at `:195-196` also matches prototypes in headers, and the C rows here list definitions only (decision 32). The C++ member functions are `method`, and the out-of-class form uses `qualified_identifier` (decision 31). Positional names (`record`, `annotation_type`, the C# `struct` and `record`, and the C++ `namespace` name) are not confirmed by any fixture yet, so the fixtures decide them under decision 22. The Kotlin class row requires `class_body` (decision 35).
- Fixtures and expected rows, written before the queries. Each row is `kind name start end depth`.
  - `testdata/java/Greeter.java`, lines: 1 `public class Greeter {`; 2 `    public Greeter() {}`; 3 `    public String hi() { return "hi"; }`; 4 `}`; 5 `interface Named {}`; 6 `enum Mode { A }`; 7 `record Point(int x, int y) {}`. Expected: `class Greeter 1 4 0`; `method Greeter 2 2 1`; `method hi 3 3 1`; `interface Named 5 5 0`; `enum Mode 6 6 0`; `class Point 7 7 0`.
  - `testdata/c/sample.c`, lines: 1 `struct Pt {`; 2 `    int x;`; 3 `};`; 4 `typedef int Count;`; 5 `int add(int a, int b) {`; 6 `    return a + b;`; 7 `}`; 8 `char *label(void) {`; 9 `    return 0;`; 10 `}`; 11 `int proto(void);`. Expected: `type Pt 1 3 0`; `type Count 4 4 0`; `function add 5 7 0`; `function label 8 10 0`. The prototype at line 11 is absent.
  - `testdata/cpp/sample.cpp`, lines: 1 `int add(int a, int b) {`; 2 `    return a + b;`; 3 `}`; 4 `class Box {`; 5 `public:`; 6 `    void open() {}`; 7 `};`; 8 `void Box::reset() {}`; 9 `namespace util {`; 10 `}`. Expected: `function add 1 3 0`; `class Box 4 7 0`; `method open 6 6 1`; `method reset 8 8 0`; `module util 9 10 0`.
  - `testdata/c_sharp/Sample.cs`, lines: 1 `public class Greeter {`; 2 `    public Greeter() {}`; 3 `    public string Hi() { return "hi"; }`; 4 `}`; 5 `public interface INamed {}`; 6 `public enum Mode { A }`; 7 `public struct Pt { public int X; }`; 8 `public record Dot(int X);`. Expected: `class Greeter 1 4 0`; `method Greeter 2 2 1`; `method Hi 3 3 1`; `interface INamed 5 5 0`; `enum Mode 6 6 0`; `type Pt 7 7 0`; `class Dot 8 8 0`.
  - `testdata/kotlin/Sample.kt`, lines: 1 `class Box {`; 2 `    fun open(): Int = 1`; 3 `}`; 4 `interface Named`; 5 `enum class Mode { A, B }`; 6 `object Store`; 7 `typealias Id = Int`. Expected: `class Box 1 3 0`; `method open 2 2 1`; `interface Named 4 4 0`; `enum Mode 5 5 0`; `constant A 5 5 1`; `constant B 5 5 1`; `class Store 6 6 0`; `type Id 7 7 0`.
- Hostile-input tests, in `internal/syntax/syntax_test.go`. Each runs the call in a goroutine and waits with `select` on `time.After(10 * time.Second)`. `TestSymbolsCPPIssue1558`: input is the 20 bytes `;e s d>f(T);template` as C++ source. Expected: `Report.Stopped` is true and there are no symbols. Source: issue #1558. `TestSymbolsOneMiBSingleLine`: input is the line `package p`, a line `var s = "`, then 1 MiB of letter `a` with no closing quote, all as Go source. Expected: the error is nil, and `Report.Stopped` or `Report.SyntaxErrors` is true. Source: decisions 9 and 16. `TestSymbolsDeeplyNestedBrackets`: input is `package p` followed by 200,000 bytes of `(`, as Go source. Expected: the error is nil, and `Report.Stopped` or `Report.SyntaxErrors` is true. Source: the work limits of decision 10.
- Tests that fail before the change: the five fixture tests `TestSymbolsFixtureJava`, `TestSymbolsFixtureC`, `TestSymbolsFixtureCPP`, `TestSymbolsFixtureCSharp` and `TestSymbolsFixtureKotlin`, and the three hostile tests above.
- Specification text: none in this step.
- Lands after: Code symbols step 2. Definition queries for TypeScript, TSX, JavaScript, Python and Rust

