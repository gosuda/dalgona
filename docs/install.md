# Install dal and dalgona from source (Linux, macOS, Windows)

Build dal and dalgona from source. Dal is the binary `dalgon`, with `dal` and `dl` as aliases. Dalgona is the binary `dalgona`, with `dg` as alias. After the build, run the [quickstart](quickstart.md) to sign in and run your first prompt.

No tagged release exists. No `dalgon-v*` or `dalgona-v*` tag has been created. The workspace version is `0.1.0`, but no tag exists for it. The registry and installer channels have nothing to fetch yet. When the first tags exist, those channels serve the same binaries.

## Build from source

You need git, the latest stable Rust toolchain, and a C toolchain. The symbol grammars compile through `cc`.

```sh
git clone https://github.com/gosuda/dalgona
cd dalgona
rustup update stable --no-self-update
cargo build --release --locked -p dalgon -p dalgona
export PATH="$PWD/target/release:$PATH"
```

In PowerShell, run the same first four commands. Then extend `PATH` with this line instead of `export`:

```powershell
$env:PATH = "$PWD\target\release;$env:PATH"
```

The binaries land in `target/release/`. The default Cargo target directory is assumed. `CARGO_TARGET_DIR` and `--target-dir` move it. The `PATH` line changes only the current shell. Add it to your shell profile to keep it. It puts `target/release` first, so these binaries win over another program named `dal`, `dl`, or `dalgon`.

Check the install:

```sh
dalgon --version
dalgona --version
```

`dalgon --version` prints `dalgon` and the package version. `dal` and `dl` print the same text. `dalgona --version` prints `dalgona` and the package version.

Source builds use the latest stable Rust toolchain. Run `rustup update stable --no-self-update` before building. The repository uses one root Cargo workspace and one `Cargo.lock`. It does not declare a minimum supported Rust version. Plugins need no toolchain.

## Platforms

Dal and dalgona build on Linux, macOS, and Windows on x86_64 and aarch64. The first prebuilt release binaries will arrive with the first tagged release. They will be unsigned in v0. Windows SmartScreen may warn about an unsigned binary. Verify the checksum where one is published before you run the binary.

## Config and data roots

Dal and dalgona use the same XDG-style layout on every OS. Only the way they find your home directory differs by OS.

| | dal (`dalgon`, `dal`, `dl`) | dalgona (`dalgona`, `dg`) |
|---|---|---|
| config file | `$XDG_CONFIG_HOME/dal/dal.toml` | `$XDG_CONFIG_HOME/dalgona/dal.toml` |
| data root | `$XDG_DATA_HOME/dal/` | `$XDG_DATA_HOME/dalgona/` |

Dal uses an `XDG_*` variable only when it is set to an absolute path. Otherwise it uses `.config` or `.local/share` under your home directory. A missing config file is valid. Dal then uses its defaults.

Your home directory comes from these variables:

| OS | Home directory |
|---|---|
| Linux and macOS | `HOME`, which must be an absolute path |
| Windows | The first absolute path of `HOME`, then `USERPROFILE`, then `HOMEDRIVE` joined with `HOMEPATH` |

If none applies, dal stops. On Linux and macOS it prints `dalgon: cannot find the home directory: HOME is not set`. On Windows it prints `dalgon: cannot find the home directory: none of HOME, USERPROFILE, or HOMEDRIVE+HOMEPATH is set`. Dal does not read `%APPDATA%` for config or data.

On Windows with `USERPROFILE` set to `C:\Users\alice` and no `HOME` or `XDG_*` variable, the paths are:

| item | path |
|---|---|
| config file | `C:\Users\alice\.config\dal\dal.toml` |
| data root | `C:\Users\alice\.local\share\dal` |
| plugin directory | `C:\Users\alice\.local\share\dal\plugins\<name>` |

Create a plugin directory in PowerShell under the same assumption:

```powershell
$d = Join-Path $env:USERPROFILE ".local\share\dal\plugins\quality"
New-Item -ItemType Directory -Force $d
```

Sign-in credentials live in `auth.json` in the data root. Dalgona has its own data root, so it has its own sign-in. [Settings in dal.toml](config.md) lists the keys of `dal.toml`.

## Next

- [Quickstart](quickstart.md) signs in and runs your first prompt.
- [Settings in dal.toml](config.md) lists every config key.
- [Plugins](plugins.md) shows how to write a plugin in the data root.
