#### Code symbols step 2. Definition queries for TypeScript, TSX, JavaScript, Python and Rust

Anchor: Christopher Alexander

- Files: new `internal/syntax/queries/typescript.scm`, `tsx.scm`, `javascript.scm`, `python.scm` and `rust.scm`. `internal/syntax/language.go`: five registry rows, each with `//go:embed` on its query. The grammar packages are `grammars/typescript` (package `typescript`), `grammars/tsx`, `grammars/javascript`, `grammars/python` and `grammars/rust`. The extension rows are `.ts`, `.mts` and `.cts` for typescript, `.tsx` for tsx, `.js`, `.jsx`, `.mjs` and `.cjs` for javascript, `.py` for python, and `.rs` for rust. Fixtures: `internal/syntax/testdata/typescript/definitions.ts`, `testdata/tsx/definitions.tsx`, `testdata/javascript/definitions.js`, `testdata/python/definitions.py` and `testdata/rust/definitions.rs`. `internal/syntax/syntax_test.go`: five fixture tests. `THIRD_PARTY_NOTICES.md`: five sections, for typescript, tsx, javascript, python and rust.
- `typescript.scm`, exactly. `tsx.scm` holds the same text:

```
(function_declaration (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(method_definition (property_identifier) @name) @definition.method
(method_definition (private_property_identifier) @name) @definition.method
(class_declaration (type_identifier) @name) @definition.class
(abstract_class_declaration name: (type_identifier) @name) @definition.class
(interface_declaration (type_identifier) @name) @definition.interface
(enum_declaration (identifier) @name) @definition.enum
(type_alias_declaration (type_identifier) @name) @definition.type
(internal_module name: (identifier) @name) @definition.module
(module name: (identifier) @name) @definition.module
(program (lexical_declaration (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @definition.function))
(export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @definition.function))
```

The first five rows are the library's inferred TypeScript rows (`grammars/tags_query_infer.go:157-165`). The enum row maps enums to `enum`, where the library maps them to `type`. The rows for private methods, abstract classes, generators, namespaces and `const` bindings of arrow functions are not in the library table, so the upstream node types name them (node types at the pinned ref of the typescript grammar, `licenses/grammars.json`). The fixture confirms each row. A row that the fixture does not confirm is deleted with its fixture lines.
- `javascript.scm`, exactly:

```
(function_declaration (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(method_definition (property_identifier) @name) @definition.method
(method_definition (private_property_identifier) @name) @definition.method
(class_declaration (identifier) @name) @definition.class
(program (lexical_declaration (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @definition.function))
(export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @definition.function))
```

The first three rows follow `grammars/tags_query_infer.go:150-156`. The others are as in typescript.
- `python.scm`, exactly:

```
(module (function_definition name: (identifier) @name) @definition.function)
(module (class_definition name: (identifier) @name) @definition.class)
(module (decorated_definition definition: (function_definition name: (identifier) @name)) @definition.function)
(module (decorated_definition definition: (class_definition name: (identifier) @name)) @definition.class)
(class_definition body: (block (function_definition name: (identifier) @name) @definition.method))
(class_definition body: (block (decorated_definition definition: (function_definition name: (identifier) @name)) @definition.method))
(class_definition body: (block (class_definition name: (identifier) @name) @definition.class))
(class_definition body: (block (decorated_definition definition: (class_definition name: (identifier) @name)) @definition.class))
```

The library's Python rows at `grammars/tags_query_infer.go:144-148` are unfielded, and they would list a method as a function. The fields `name`, `body` and `definition` and the root `module` are the upstream node types at the pinned ref (`licenses/grammars.json`). A decorated definition spans its decorators (decision 34). Module-level assignments are not listed.
- `rust.scm`, exactly:

```
(source_file (function_item name: (identifier) @name) @definition.function)
(mod_item body: (declaration_list (function_item name: (identifier) @name) @definition.function))
(impl_item body: (declaration_list (function_item name: (identifier) @name) @definition.method))
(trait_item body: (declaration_list (function_signature_item name: (identifier) @name) @definition.method))
(trait_item body: (declaration_list (function_item name: (identifier) @name) @definition.method))
(struct_item name: (type_identifier) @name) @definition.type
(union_item name: (type_identifier) @name) @definition.type
(type_item name: (type_identifier) @name) @definition.type
(enum_item name: (type_identifier) @name) @definition.enum
(trait_item name: (type_identifier) @name) @definition.interface
(mod_item name: (identifier) @name) @definition.module
(const_item name: (identifier) @name) @definition.constant
(static_item name: (identifier) @name) @definition.variable
```

The library's Rust rows are at `grammars/tags_query_infer.go:183-193`. The function rows are anchored to `source_file`, `mod_item` and the bodies of `impl_item` and `trait_item`, so that each function has one kind (the library would give a method one kind and a second one through the same row). `macro_definition` is not listed (decision 37).
- Fixtures and expected rows, written before the queries. Each row is `kind name start end depth`.
  - `testdata/typescript/definitions.ts`, lines: 1 `export function greet(name: string): string {`; 2 `  return name;`; 3 `}`; 4 blank; 5 `export class Box {`; 6 `  open(): void {}`; 7 `  private seal(): void {}`; 8 `}`; 9 blank; 10 `export abstract class Base {`; 11 `  abstract run(): void;`; 12 `}`; 13 blank; 14 `export interface Shape {`; 15 `  area(): number;`; 16 `}`; 17 blank; 18 `export enum Color {`; 19 `  Red,`; 20 `}`; 21 blank; 22 `export type Pair = [number, number];`; 23 blank; 24 `export const twice = (n: number) => n * 2;`; 25 blank; 26 `export namespace Util {`; 27 `  export const v = 1;`; 28 `}`. Expected: `function greet 1 3 0`; `class Box 5 8 0`; `method open 6 6 1`; `method seal 7 7 1`; `class Base 10 12 0`; `interface Shape 14 16 0`; `enum Color 18 20 0`; `type Pair 22 22 0`; `function twice 24 24 0`; `module Util 26 28 0`. The abstract method at line 11 is not listed. Source: the typescript rows of this step.
  - `testdata/tsx/definitions.tsx`, lines: 1 `export function Card(props: { title: string }) {`; 2 `  return <div>{props.title}</div>;`; 3 `}`; 4 blank; 5 `export const Button = () => <button>ok</button>;`; 6 blank; 7 `export interface CardProps {`; 8 `  title: string;`; 9 `}`. Expected: `function Card 1 3 0`; `function Button 5 5 0`; `interface CardProps 7 9 0`.
  - `testdata/javascript/definitions.js`, lines: 1 `function add(a, b) {`; 2 `  return a + b;`; 3 `}`; 4 blank; 5 `class Counter {`; 6 `  bump() {`; 7 `    return 1;`; 8 `  }`; 9 `  #hide() {}`; 10 `}`; 11 blank; 12 `const scale = (n) => n * 2;`; 13 blank; 14 `function* gen() {`; 15 `  yield 1;`; 16 `}`. Expected: `function add 1 3 0`; `class Counter 5 10 0`; `method bump 6 8 1`; `method #hide 9 9 1`; `function scale 12 12 0`; `function gen 14 16 0`.
  - `testdata/python/definitions.py`, lines: 1 `def load(path):`; 2 `    return path`; 3 blank; 4 blank; 5 `class Store:`; 6 `    def get(self, key):`; 7 `        def inner():`; 8 `            return key`; 9 `        return inner()`; 10 blank; 11 `@cache`; 12 `def fetch(url):`; 13 `    return url`. Expected: `function load 1 2 0`; `class Store 5 9 0`; `method get 6 9 1`; `function fetch 11 13 0`. The nested `inner` is absent.
  - `testdata/rust/definitions.rs`, lines: 1 `pub fn start() {}`; 2 blank; 3 `pub struct Config {`; 4 `    pub name: String,`; 5 `}`; 6 blank; 7 `pub enum Mode {`; 8 `    Fast,`; 9 `}`; 10 blank; 11 `pub trait Runner {`; 12 `    fn run(&self);`; 13 `}`; 14 blank; 15 `impl Config {`; 16 `    pub fn new() -> Config {`; 17 `        Config { name: String::new() }`; 18 `    }`; 19 `}`. Expected: `function start 1 1 0`; `type Config 3 5 0`; `enum Mode 7 9 0`; `interface Runner 11 13 0`; `method run 12 12 1`; `method new 16 18 0`.
- Tests that fail before the change: `TestSymbolsFixtureTypeScript`, `TestSymbolsFixtureTSX`, `TestSymbolsFixtureJavaScript`, `TestSymbolsFixturePython` and `TestSymbolsFixtureRust`, all in `internal/syntax/syntax_test.go`. Each reads its fixture, calls `Symbols`, and compares the rows with the expected table literal of this step.
- Specification text: none in this step.
- Lands after: Code symbols step 1. Syntax package boundary and Go definitions

