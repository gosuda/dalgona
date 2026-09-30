# Install dal and dalgona

Binaries: `dal`, `dal`, `dl`, `dalgona`, `dg`. The command is `dal`; `dal` and `dl` mean the same.

Channels:

```sh
cargo install dal
cargo install dalgona
cargo binstall dal
cargo binstall dalgona
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/gosuda/dalgona/releases/download/dalgon-v0.1.0/dalgon-installer.sh | sh
```

```powershell
powershell -c "irm https://github.com/gosuda/dalgona/releases/download/dalgon-v0.1.0/dalgon-installer.ps1 | iex"
```

Dalgona substitutes tag `dalgona-v0.1.0` and installer name `dalgona-installer`.

Platforms: Linux, macOS, Windows on x86_64 and aarch64. `cargo install` from source needs a C toolchain because the symbols grammars compile through `cc`; `cargo binstall` and the dist installers use prebuilt release binaries. If another package already installed `dal`, `dl`, or `dalgon`, Cargo refuses to overwrite without `--force`; use `cargo install dalgon --bin dalgon` for only the long name or `cargo install dalgon --force` to replace all three. Dist installers install all three real binaries to one PATH directory; `$CARGO_HOME` unset falls back to `$HOME/.cargo/bin`. Windows binaries are unsigned in v0 and may trigger SmartScreen; verify the release checksum before running them.

Config and data roots per OS follow the CLI page. Plugins need no toolchain.
