# Embed dal in a Rust program

Use `dal-agent` and `dal-core` through their public Rust APIs. The repository has one Cargo workspace and one `Cargo.lock`.

## Run the embedding example

From the repository root, run:

```sh
cargo run -p dalgon --example sdk
```

The example uses a local replay fixture. It needs no provider credentials. It prints `hello from scripted SDK`. Then it shuts down the host. Read the complete implementation in [`cli/dalgon/examples/sdk/main.rs`](../cli/dalgon/examples/sdk/main.rs).

## Depend on dal from your own project

No tagged release exists yet, and the registry has nothing to fetch (see [Install dal and dalgona](install.md)). Depend on the repository instead. Pin `rev` to a commit you tested. Use the latest stable Rust toolchain.

```text
# Cargo.toml of your project
[dependencies]
dal-agent = { git = "https://github.com/gosuda/dalgona", rev = "<commit>" }
dal-core = { git = "https://github.com/gosuda/dalgona", rev = "<commit>" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The `tokio` line serves the example, which uses `#[tokio::main(flavor = "multi_thread")]`. In a local checkout, replace each `git` and `rev` pair with a `path` to `engine/dal-agent` or `core/dal-core`.

## Own the session lifecycle

Load configuration with `Config::load(ConfigProduct::Dalgon, data_root, product_defaults_toml, user_toml)`. The loader takes TOML text. It does not read files or the environment. Capture those inputs at your process boundary.

Build a `Product` with its data root and extensions. Pass it with the configuration and captured `Env` to `Host::start`. Open a session with `Host::open`. Use `SessionRef::Ephemeral` when the session must not be saved. `Host::close` closes one session by id. It closes descendant child sessions before their parent.

Subscribe before submitting a prompt. Handle a rejected submission, a closed subscription, and a resync request. Read text updates until `UpdateKind::TurnEnded`. Do not wait for the subscription to close before calling `Host::shutdown`. `Host::shutdown` takes a timeout grace duration. It returns a report with the closed session count. The runnable example shows each of these outcomes.

## Store extension state

In saved sessions, file-backed extensions use the session sidecar service for small private values. Sidecars are unavailable in ephemeral sessions, including the example above. Names must contain 1 to 64 bytes of lowercase ASCII letters, digits, `.`, `_`, or `-`. They must start with a letter or digit. Sidecar files stay inside the owning extension's session directory. Writes are atomic. The `judge` service lets a cell ask the session judge a question.

## Exchange waiting mail between sessions

The shared agents service carries mailbox messages for dal and composed dalgona. There is one mechanism, not one per product.

Declare only the agents operations the caller needs. The host mints the caller identity and grants that service. A caller uses the capability-scoped services handle. It never writes the journal directly.

A recipient holds at most 100 waiting `NextTurn` messages. Delivered messages stay as history and do not count toward that bound. The next eligible user turn consumes waiting messages and writes them as turn input in one durable journal record. A new message can queue after that delivery.

Reads use a cursor and leave history in place. Reopening the session revalidates the delivery record. A full mailbox refuses with `Full` and writes no journal row. A missing or finished recipient answers `Gone`. A finished child has no promise of delivery.

Name the mode with `MailMode::NextTurn` and read the answer from `Receipt` (`Buffered`, `Full`, `Gone`). The bound is `MAILBOX_WAITING_LIMIT`.

## Next

- [Protocol](protocol.md): serve the same operations to other processes.
- [Build a client on the dal protocol](integrate.md): connect a program through `dalgon rpc`.
- [Plugins](plugins.md): extend dal in Starlark without a Rust build.
