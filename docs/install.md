# Install dal and dalgona

Binaries: `dalgon` (primary, with `dal` and `dl` as aliases) and `dalgona` (with `dg` as alias).

Build from source. No tagged release exists, so the registry and installer channels have nothing to fetch yet.

```sh
git clone https://github.com/gosuda/dalgona
cd dalgona
rustup update stable --no-self-update
cargo build --release -p dalgon
cargo build --release -p dalgona
```

The binaries land in `target/release/`. When the first `dalgon-v*` and `dalgona-v*` tags land, the registry and installer channels serve the same binaries.

Source builds use the latest stable Rust toolchain. Run
`rustup update stable --no-self-update` before building. The repository uses one
root Cargo workspace and one `Cargo.lock`; it does not declare a minimum
supported Rust version.

Platforms: Linux, macOS, Windows on x86_64 and aarch64. Building from source needs a C toolchain because the symbols grammars compile through `cc`; prebuilt release binaries arrive with the first tagged release. If another package already installed `dal`, `dl`, or `dalgon`, Cargo refuses to overwrite without `--force`. Windows binaries are unsigned in v0 and may trigger SmartScreen; verify the release checksum before running them.

Config and data roots per OS follow [Settings in dal.toml](config.md). Plugins need no toolchain.
