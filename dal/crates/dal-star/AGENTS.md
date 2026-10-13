# dal-star authoring contract

Starlark composes approved operations and transforms their results. The host
owns authority and resource enforcement. Shared libraries are frozen;
independent evaluations do not share mutable request state.

Apply this file to every `plugin.star`, eval cell, and `dal` SDK change in this
crate. These rules bind both script authors and the Rust host.

## 1. Define the dialect explicitly

"Valid Starlark" is not a compatibility contract. The Rust implementation
exposes dialect options and extensions that differ from the standard language.

- Keep the runtime version, dialect settings, and host API version pinned
  together. This crate's dialect is `Dialect::Standard` plus
  `enable_top_level_stmt`, `enable_positional_only_arguments`, `enable_f_strings`,
  and `enable_types: ParseOnly` (`engine.rs`); do not change it per call site.
- Use type annotations for maintained plugin interfaces. Keep generated
  one-shot snippets lightweight.
- Do not assume Python syntax or stdlib functions exist because the code looks
  familiar. Starlark has no `is` operator: use `value == None`, never `is`.
- For portable code prefer `.format()` over f-strings: the Rust f-string
  extension has restricted interpolation syntax, so treating it as Python's is
  a mistake.

## 2. Keep mutation local

Build fresh results using local mutable containers. Do not maintain mutable
module-level state.

- Lists and dicts are mutable during construction; freezing makes shared module
  data immutable so initialized modules can be reused safely. A function that
  mutates a module-level cache can work during initialization and fail after
  the module is frozen.
- Good default shape: a function takes inputs, builds a local list or dict, and
  returns a fresh result without touching its input. Example:

  ```starlark
  def matching_paths(paths, suffix = ".rs"):
      """Return sorted, unique matching paths without changing the input."""
      matched = {}
      for path in paths:
          if path.endswith(suffix):
              matched[path] = True
      return sorted(matched)
  ```

  Precondition: `paths` is a finite sequence of strings, `suffix` a string.
  Postcondition: each matching path appears once, sorted; input unchanged.
- Avoid mutable parameter defaults such as `def collect(items = [])`. Defaults
  evaluate at definition time: mutations persist between calls before freezing
  and the default becomes immutable afterward. Use an immutable default or
  allocate the mutable value inside the function.

## 3. Prefer obvious operations over clever abstractions

- Building a list: use a local list with `append()` or `extend()`, not repeated
  concatenation.
- Deduplication: use dictionary keys rather than repeatedly searching a growing
  list.
- Comprehensions: use for straightforward transformations; put tool calls in
  explicit statements or loops.
- Function interfaces: prefer named parameters over arbitrary configuration
  dictionaries and catch-all `**kwargs`.
- Abstraction: extract a meaningful invariant or reused operation, not a wrapper
  that merely renames one call.
- Optional values: distinguish missing data from valid empty data. Index for
  required fields (`record["path"]`), use `.get()` only for fields a schema
  permits to be absent (`record.get("description", "")`). Do not `.get()`
  everything as a substitute for a schema.
- Style: `snake_case`, four-space indentation, docstrings on maintained public
  functions — the official Bazel `.bzl` style.
- Validate untrusted input at the host boundary. Once the host guarantees a
  field's type and presence, do not repeat the check inside the script.

## 4. Design failure semantics before adding tool APIs

Starlark has no `try/except`; `fail()` aborts evaluation. Distinguish expected
outcomes from actual errors.

- Successful file read: return the content directly.
- Search finds no matches: return a valid empty result.
- Search stops at its limit: return the partial result with explicit
  truncation metadata.
- Command exits unsuccessfully: return exit status and bounded output when
  inspecting that status is part of the operation.
- Permission denial or invalid arguments: abort with a precise host error.
- Unexpected host failure: abort; do not disguise it as an empty result.

When an expected failure genuinely requires branching, expose a specific
outcome for that operation; do not impose a generic `Result` wrapper and
`.unwrap()` ceremony on every call. Keep limits inside the tool call —
`tools.search(limit = ...)` — rather than retrieving everything and slicing.

## 5. Cache frozen libraries, not request state

Use a fresh execution environment for each independent evaluation. Reuse
frozen libraries.

- Freezing is an explicit `Module` operation: returning from evaluation does
  not freeze a module for sharing.
- Library initialization is side-effect-free: no commands, no reading changing
  workspace files, no capturing credentials. Resolve imports through an
  approved module registry or bounded loader.
- Per-run state stays per-run: input data, cancellation state, permissions,
  and tool-call accounting live outside cached library state. A cached native
  function consults the current evaluation context, never a previous request's
  authority.
- Cache identity covers dependencies: module content, transitive imports,
  dialect configuration, and host API compatibility. Bound the cache.
- On the Rust side, do not hold `Module` values past the module's release
  without the appropriate frozen heap; convert boundary results into bounded,
  owned host data rather than extending lifetimes.
- Do not use `Evaluator::eval_statements` in production session management:
  upstream documents it as a debugging facility with significant caveats.

## 6. Enforce authority and resource limits outside the script

A restricted language is not an operating-system sandbox. The evaluator's
`set_max_tick_count`, `set_max_callstack_size`, and `set_max_heap_size`
(invoke.rs) bound calls and loop backedges, not CPU time, and the heap limit
does not cover all native allocations — upstream warns against treating these
limits as protection against truly malicious code.

Cover the whole operation in the host:

- Source and module loading: source-size limits, bounded imports, approved
  module resolution.
- Interpreter execution: tick, call-depth, heap, and cancellation limits.
- Native tool calls: authorization on every call, deadlines, call-count
  limits, bounded response sizes.
- Worker process: OS-enforced resource limits and restricted ambient access.
- Output and diagnostics: byte and nesting limits; safe handling of cyclic or
  unsupported values.

A script may reduce its authority, never increase it. A `pure` mode removes
effectful capabilities; it is not a label the host trusts without enforcement.
Do not authorize by scanning source text for forbidden names: authorization
belongs in the host operation, after arguments are validated. Define
partial-effect behavior explicitly: an evaluation error does not roll back
earlier writes or commands. Record completed effects; do not blindly retry a
failed evaluation that may already have changed external state.

## 7. Be precise about determinism

The standard language is deterministic and hermetic by default; host functions
you add do not inherit that property. Distinguish deterministic computation
from reproducible tool execution.

- A pure helper can be tested against fixed inputs.
- A script calling `tools.read()` observes a changing workspace; a script
  calling `tools.exec()` may observe much more.
- Record ordered tool calls and their bounded responses for replay. Cache pure
  transformations freely when inputs are fully identified; do not cache
  effectful evaluations as though they were pure functions.
- Sort output when its contract is set-like; preserve order when it represents
  priority, ranking, or command sequencing.

## 8. Use Starlark-aware tooling and test the real lifecycle

- Lint `.star` files with Buildifier's generic file type:
  `buildifier --type=default --mode=check --lint=warn plugin.star`. When
  Rust-specific syntax is enabled, verify the pinned formatter accepts it;
  formatter acceptance is not parsing.
- Use `AstModule::lint` with the known global names to catch references to
  nonexistent APIs; pin its version since lint checks change between releases.
- Prioritize: frozen-module tests (load, freeze, call exported functions
  repeatedly, inputs unchanged); contract tests (empty results, missing
  required fields, explicit truncation, expected command failures); and
  host-boundary tests (denied operations, cancellation during native calls,
  oversized data, failure after an earlier effect completed).

## Sources

- `Dialect`, `Module`, `Evaluator`, `AstModule`: https://docs.rs/starlark/latest/starlark/
- Language spec: https://github.com/bazelbuild/starlark/blob/master/spec.md
- Design (freezing, mutation): https://github.com/bazelbuild/starlark/blob/master/design.md
- Style: https://bazel.build/rules/bzl-style
- Buildifier: https://github.com/bazelbuild/buildtools/blob/master/buildifier/README.md
