# Quickstart: run your first dal agent turn

Dal is a coding agent for your terminal. This page takes you from a source build to one answered prompt and one approved edit.

You sign in once. Dal stores the credentials and reuses them in every workspace. Dal asks before it edits a file or runs a command, so you can try it on a real repository.

## Before you start

You need:

- git.
- The latest stable Rust toolchain from rustup.
- A C toolchain. The symbol grammars compile through `cc`.
- An account or an API key for one provider.

## Install

Build from source. No tagged release exists, so the registry and installer channels have nothing to fetch yet. [Install](install.md) lists the platform details.

```sh
git clone https://github.com/gosuda/dalgona
cd dalgona
rustup update stable --no-self-update
cargo build --release --locked -p dalgon -p dalgona
export PATH="$PWD/target/release:$PATH"
```

In PowerShell, use `$env:PATH = "$PWD\target\release;$env:PATH"` for the last line.

Check the install. `dalgon --version` prints `dalgon` and the package version. `dal` and `dl` are aliases of `dalgon`.

```sh
dalgon --version
```

## Sign in

Run the sign-in command in a terminal.

```sh
dalgon login
```

Dal lists the providers it can sign in to. Type the number or the id of one provider. Press Enter. An empty line cancels.

- A provider with browser or device sign-in prints a URL. A device sign-in also prints a code. Follow the lines dal prints. Dal waits until the sign-in ends.
- A provider with an API key asks for the key. Dal masks what you type. It then prints `Saved <name> credentials. Run dal to pick a model.`

To sign in without a prompt, pipe the key and name the provider:

```sh
printf %s "$KEY" | dalgon login anthropic --api-key
```

A provider also counts as signed in when its key variable is set, such as `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`.

Check the result:

```sh
dalgon login status
```

Each line names a provider and its state: `ready`, `expired`, or `not configured`. The command exits 0 when at least one provider is ready. Otherwise it exits 1 and prints `Run dalgon login <provider> to sign in.` on stderr.

Dalgona keeps its own data root, including its sign-in. Run `dalgona login` to sign it in.

## Pick a model

Dal needs a model before it can answer. Start dal in your project directory.

```sh
cd /path/to/your/project
dalgon
```

Dal shows `No model selected yet. dal needs a model before it can answer.` Type `/model` and press Enter. Dal lists the models of the providers you signed in to. Type to filter the list. Move with the arrow keys. Press Enter to pick a model. Press Esc to cancel. Dal saves your pick as the default model in `dal.toml`.

To pick a model without the terminal interface, list the models. Then pass one as `PROVIDER/ID`, joining the first two columns of a row with a slash.

```sh
dalgon models
dalgon -m PROVIDER/ID -p "Summarize this repository's README"
```

You can also set `model = "PROVIDER/ID"` in `dal.toml`. See [Install](install.md#config-and-data-roots) for the file path and [Settings in dal.toml](config.md) for the keys.

## Run your first turn

Print mode runs one turn without the terminal interface. It writes the answer to stdout and exits.

```sh
cd /path/to/your/project
dalgon -p "Summarize this repository's README"
```

Without a model, print mode stops with `dalgon: no model is configured: dal.toml has no model key and --model was not given`. Pick a model first.

For a conversation, run `dalgon` and type a prompt. Press Enter to send. Press Esc to interrupt a running turn. Press F1 for key help. Type `/quit` to leave. [Terminal and keys](terminal.md) lists every key.

Dal saves the session. Continue it later with `dalgon -c`.

## Approve a change

The default approval mode is `ask`. Dal asks before every `patch` and every `exec`.

In the terminal, ask for a small edit:

```text
Create notes.txt that contains one line of text.
```

Before dal edits the file, it shows a prompt titled `Allow this edit to <path>?`. Press `y` to allow the edit once. Press `a` to allow for this session. Press `n` or Esc to deny. The answer keys unlock a moment after the prompt opens. The prompt also offers View full preview. A shell command gets the title `Allow this command?`.

Print mode cannot ask. Under `ask`, it denies `patch` and `exec` calls. When the model calls `patch`, print mode prints these lines on stderr:

```text
dalgon: approval is ask and no terminal is attached: patch and exec calls are denied. Re-run with --approval all to approve them.
dalgon: patch needs approval; print mode cannot ask. Rerun with --approval edits.
dalgon: 1 call denied by approval mode.
```

Pass `--approval edits` to allow edits in print mode. `exec` still needs `--approval all`. Use `all` only inside an external sandbox. The built-in `--sandbox` sets no network restriction. See [Sandbox](sandbox.md).

## What you did

You built dal, signed in, picked a model, ran one turn, and answered one approval prompt.

## Next

- [Tools, modes, and approval](tools.md) explains the approval ladder and the `eval` tool.
- [Sessions and compaction](sessions.md) covers `-c`, `-r`, and `-n`.
- [Settings in dal.toml](config.md) lists every key.
- [Command line](cli.md) lists the subcommands and exit codes.
- [Plugins](plugins.md) shows how to write your first plugin.
