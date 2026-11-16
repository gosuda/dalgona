# Contribute

## Define the change

State the required behavior, the failure your change prevents, and the affected consumer. Do not add an interface, option, dependency, or helper for a possible future need.

When you replace an implementation, migrate all callers in the same change and remove the old code. Do not leave compatibility shims, unused exports, duplicate routes, or commented-out implementations. The `unused`, `staticcheck`, and `ineffassign` checks, and the `godox` keyword check (TODO, FIXME, XXX, HACK markers), in [.golangci.yml](.golangci.yml) catch part of this rule. Review the parts that lint cannot detect, including shims, exported symbols, and commented-out code.

## Keep the implementation direct

Use names, types, and control flow to explain the implementation. Do not use comments to hide unclear logic. Write no prose comment in Go, hand-written TypeScript and JavaScript, Svelte, or Python source. Keep only these Go directives: `//go:build`, `//go:embed`, `//go:generate`, and `//nolint:<linters> // <reason>`. Non-Go directive comments are forbidden, for example `@ts-expect-error`, `biome-ignore`, `svelte-ignore`, and `noqa`. A legal notice that a license requires is legal metadata, not a prose comment, and stays where the license requires it. A third-party notice file does not replace a required source notice. Put the source of a non-obvious protocol rule in `docs/spec`, with the upstream project and file named.

Use `encoding/json/jsontext` for wire, journal, and hot-path JSON. Use `protowire` for Protobuf. You can use `encoding/json/v2` for cold config-shaped data. Use HTTP and streaming codecs for provider adapters, not provider SDKs. [The dependency decision](docs/dal/decisions/dependencies.md) and depguard in [.golangci.yml](.golangci.yml) define the import restrictions.

Avoid an allocation or copy when a slice, reused buffer, or stream can do the same work. Add a package only when it hides a real decision, not when it only forwards calls.

## Prove the behavior

Make each test detect a distinct, plausible defect. Take expected values from an independent oracle or an explicit contract. Do not assert source text, use saved output as proof, or repeat the implementation as the oracle.

Three named drift gates are the exceptions to the rule above on source text and saved output. The comment gate, `TestRepositoryHasNoProseComments` in `internal/commentgate`, MAY read source text to find prose comments. Its legal-metadata exception permits an exact registered required notice, not explanatory prose or a non-Go directive. The dependency table gate, `TestDependencyTableMatchesGoMod` in `internal/pingate`, MAY read `go.mod` and `docs/decisions/dependencies.md` to detect disagreement. The golden request gate, `compareGolden` in `internal/agent`, MAY compare a model request with its files under `internal/agent/testdata/golden`. All three gates detect drift. Their findings and goldens are never proof of behavior. `TestEncodedRequestMatchesJournalHistory` is the proof for the request history: it encodes seeded journals with the real chatcompat and Anthropic encoders and compares the result with an oracle built from the journal inputs.

Prove that a new test detects its defect with a mutation probe: put that defect into the code, for example by dropping, duplicating, or reordering one step, watch the test fail, then restore the code. Never commit the probe.

For persistence changes, test interruption and damaged input against [journal.md](docs/dal/spec/journal.md) and [recovery.md](docs/dal/spec/recovery.md). For protocol changes, preserve [protocol.md](docs/dal/spec/protocol.md) and the vendored schema tests in `internal/dal/rpc`. For edit changes, test refusal without partial writes under [edit-styles.md](docs/dal/spec/edit-styles.md).

Live debugging: `just live-up`, then follow [live-debug.md](docs/dal/spec/live-debug.md); `just live-down` stops everything.

## Use the declared tools

Use the versions pinned in [go.mod](go.mod) and [web/package.json](web/package.json). Do not weaken a gate, raise a limit, or exclude a failing test to make a change pass.

[justfile](justfile) declares `fmt-check`, `vet`, `lint`, `test`, `vuln`, `hashline-model`, `spec-style`, `web-check`, and `cf-check`. Run them together with `just verify`. `test` uses the race detector. `hashline-model` and `spec-style` run when `uv` is available. Without `uv`, they skip locally and fail when `CI=true`. Do not report those skips as a pass.

Use `just web` to install the locked web dependencies and build the assets. `web-check` uses the installed web tools. `cf-check` installs the locked Cloudflare dependencies before its checks. `just build` builds both binaries for the host into `bin/`; `just dal` and `just dalgona` run them from source and pass their arguments through. `just dist` builds release binaries, and `just box-image` builds the remote box image.

Report the checks you ran, their results, and any untested requirement. Distinguish local checks from live-provider, reboot, desktop-app, and cloud-service checks.

## Keep the delivery bounded

Ship real behavior, not stubs, placeholder implementations, debug output, or deferred required paths. Write documentation that explains a contract, an action, or a decision the reader needs. Do not add process records without a durable use.

Keep headings in sentence case and actions verb-first. Errors name what happened and how to fix it. Use no exclamation marks or moon puns in errors and status text. Keep any moon motif to at most one visual mark on a user-facing surface.

A contribution does not authorize a push, publication, deployment, credential transfer, or cloud resource deletion. Get separate authorization for each action.
