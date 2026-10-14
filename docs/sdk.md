# Embed dal in a Rust program

Use `dal-agent` and `dal-core` through their public Rust APIs. The repository has one Cargo workspace and one `Cargo.lock`.

## Run the embedding example

From the repository root, run:

```sh
cargo run -p dalgon --example sdk
```

The example uses a local replay fixture and needs no provider credentials. It prints `hello from scripted SDK` and shuts down the host. Read the complete implementation in [`cli/dalgon/examples/sdk/main.rs`](../cli/dalgon/examples/sdk/main.rs).

## Own the session lifecycle

Load configuration with `Config::load(ConfigProduct::Dalgon, data_root, product_defaults_toml, user_toml)`. The loader takes TOML text; it does not read files or the environment. Capture those inputs at your process boundary.

Build a `Product` with its data root and extensions, and pass it with the configuration and captured `Env` to `Host::start`. Open a session with `Host::open`; use `SessionRef::Ephemeral` when the session must not be saved. `Host::close` closes one session by id, closing descendant child sessions before their parent.

Subscribe before submitting a prompt. Handle a rejected submission, a closed subscription, and a resync request. Read text updates until `UpdateKind::TurnEnded`; do not wait for the subscription to close before calling `Host::shutdown`. `Host::shutdown` takes a timeout grace duration and returns a report with the closed session count. The runnable example shows each of these outcomes.

## Store extension state

In saved sessions, file-backed extensions use the session sidecar service for
small private values. Sidecars are unavailable in ephemeral sessions, including
the example above. Names must contain 1 to 64 bytes of lowercase ASCII letters,
digits, `.`, `_`, or `-`, and must start with a letter or digit. Sidecar files
stay inside the owning extension's session directory, and writes are atomic.

