# Contribute

## Define the change

State the required behavior, the failure your change prevents, and the affected consumer. Do not add an interface, option, dependency, or helper for a possible future need.

When you replace an implementation, migrate all callers in the same change and remove the old code. Do not leave compatibility shims, unused exports, duplicate routes, or commented-out implementations. The `unused`, `staticcheck`, `ineffassign`, and `godox` checks in [.golangci.yml](.golangci.yml) enforce part of this rule. Review the parts that lint cannot detect.

## Keep the implementation direct

Use names, types, and control flow to explain the implementation. Do not use comments to hide unclear logic. Keep required build directives. Use a source citation when it explains a non-obvious protocol rule.

Use `encoding/json/jsontext` for wire, journal, and hot-path JSON. Use `protowire` for Protobuf. You can use `encoding/json/v2` for cold config-shaped data. Use HTTP and streaming codecs for provider adapters, not provider SDKs. [The dependency decision](docs/decisions/dependencies.md) and depguard in [.golangci.yml](.golangci.yml) define the import restrictions.

Avoid an allocation or copy when a slice, reused buffer, or stream can do the same work. Add a package only when it hides a real decision, not when it only forwards calls.

## Prove the behavior

Make each test detect a distinct, plausible defect. Take expected values from an independent oracle or an explicit contract. Do not assert source text, use saved output as proof, or repeat the implementation as the oracle.

For persistence changes, test interruption and damaged input against [journal.md](docs/spec/journal.md) and [recovery.md](docs/spec/recovery.md). For protocol changes, preserve [protocol.md](docs/spec/protocol.md) and the vendored schema tests in `internal/rpc`. For edit changes, test refusal without partial writes under [edit-styles.md](docs/spec/edit-styles.md).

## Use the declared tools

Use the versions pinned in [go.mod](go.mod) and [web/package.json](web/package.json). Do not weaken a gate, raise a limit, or exclude a failing test to make a change pass.

[justfile](justfile) declares `fmt-check`, `vet`, `lint`, `test`, `vuln`, `hashline-model`, `web-check`, and `cf-check`. Run them together with `just verify`. `test` uses the race detector. `hashline-model` skips when `uv` is absent; do not report that skip as a pass.

Use `just web` to install the locked web dependencies and build the assets. `web-check` uses the installed web tools. `cf-check` installs the locked Cloudflare dependencies before its checks. `just dist` builds release binaries, and `just box-image` builds the remote box image.

Report the checks you ran, their results, and any untested requirement. Distinguish local checks from live-provider, reboot, desktop-app, and cloud-service checks.

## Keep the delivery bounded

Ship real behavior, not stubs, placeholder implementations, debug output, or deferred required paths. Write documentation that explains a contract, an action, or a decision the reader needs. Do not add process records without a durable use.

A contribution does not authorize a push, publication, deployment, credential transfer, or cloud resource deletion. Get separate authorization for each action.
