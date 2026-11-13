# Configuration

The program MUST read `config.toml` and then `settings.toml` from its product home. Settings MUST override config values. Command flags MUST override both files. Defaults MUST apply when no layer sets a value.

`dal` uses `$DAL_HOME` or `~/.dal`. `dalgona` uses `$DALGONA_HOME` or `~/.dalgona`. The loader MUST expose the resolved home. The resolved home MUST be an absolute path. A relative home MUST resolve against the directory the process started in. The two homes MUST remain separate. On Windows the `dal` host start and the dalgona daemon start MUST reset the protected DACL on the product home. The same reset MUST apply to every file and directory inside the home. The same reset MUST NOT change a symlink or an irregular entry.

The loader MUST reject unknown keys. The error MUST report the file and its 1-based line. `dal` MUST reject `runner`, `remote`, `runners`, `remotes`, `mcp_servers`, `mcp_trusted_workspaces`, `discovery`, `herdr`, `web`, and `edit_review` as dalgona-only settings. Presence MUST trigger this rejection, including an empty table or a false value. `tool_mode` and `[eval]` MUST load in both products.

If a layer cannot be read or fails validation, `Load` MUST return an error.

The default approval mode is `auto`. The default sandbox is `workspace-write`. The default tool mode is `eval-first` for both products. `tool_mode = "normal"` restores the full tool list. The default runner is `local`. The default reasoning level is `default`.

When no layer sets `edit_style`, every model MUST use the edit style `anchor`.

The approval mode MUST be the top-level scalar `approval`. Its allowed values are `auto`, `yolo`, and `ask`. The decider settings MUST use `[approval_decider]`:

```toml
approval = "auto"

[approval_decider]
decider = "decisions:typesafe/jev-1.14"
threshold = 0.9
```

`approval_cfg` and `[approval]` are not valid configuration keys.

The decider MUST start with `decisions:` or `llm:`. The threshold MUST be in `(0, 1]`. Validation errors MUST name `[approval_decider] decider` or `[approval_decider] threshold`.

The `[edit_review]` table MUST set the opt-in edit review, and it MUST be dalgona only. Its key `enabled` MUST be a boolean, and it defaults to `false`.

```toml
[edit_review]
enabled = true
feedback = ["lost_code", "placeholder", "incomplete"]
```

An absent `feedback` MUST select the built-in speaker set. An empty `feedback` MUST select no speaker. The `sound` question MUST NOT be a speaker.

The interactive settings MUST use the keys in the table below.

| Key | Values | Default | Wire name |
|---|---|---|---|
| `verbosity` | `low`, `medium`, `high` | `medium` | `verbosity` |
| `theme` | `auto`, `auto-soft`, `night`, `soft-night`, `day`, `soft-day` | `auto` | `theme` |
| `icons` | `auto`, `unicode`, `nerd` | `auto` | `icons` |
| `status_hide` | `effort`, `approval`, `sandbox`, `folder`, `queue`, `context`, `mark` | `[]` | `statusHide` |
| `notifications` | `true`, `false` | `true` | `notifications` |

The `auto` theme MUST follow the system and use `night` or `day`. The `auto-soft` theme MUST follow the system and use `soft-night` or `soft-day`. The `status_hide` key MUST NOT name `model`.

`Host.WriteSettings` MUST apply each interactive setting to the running host at once. A rejected edit MUST leave the running settings and `settings.toml` unchanged. A saved `approval` or `sandbox` MUST NOT change a thread that is already running. Each thread keeps the values that its journal records.
Live setting writes MUST apply the same product restrictions as file loading. `dal` MUST reject dalgona-only roots before persistence, including `edit_review.enabled`.

When no layer sets `[approval_decider] threshold`, its value MUST be `0.9`.

The loader MUST validate approval, sandbox, reasoning, tool mode, durations, and runner options. Durations MUST use Go duration syntax. Invalid durations MUST fail loading. A sandbox0 runner MUST use a template and MUST NOT set an image.

A `[remotes.<name>]` table records a daemon in a box (Mode B, see `runners.md`). Its keys are `service`, `image`, `template`, `url`, and `box_id`. Each key MUST merge across layers on its own. A key present in a later layer MUST replace the earlier value, including an empty string. An absent key MUST keep the earlier value. Other keys inside the table MUST fail loading as unknown keys.

Edit styles MUST be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, or strict. A scalar style MUST apply to every model. A style table MUST contain `default`. Model patterns MUST match the full model name without regard to case. `*` matches any run and `?` matches one character. The first row in TOML source order wins.

A custom keyword name MUST match `^[a-z][a-z0-9_-]{1,31}$`. A custom keyword MUST set exactly one of `notice` or `text`. A notice path MUST be relative to the product home and MUST name an existing regular file. A built-in keyword override MAY set only selected fields, including `enabled = false`.

`WriteSettings` MUST update only `settings.toml`. It MUST apply all of its edits, or none. It MUST preserve `config.toml`. It MUST preserve the source order of existing `edit_style` rows. It MUST decode and validate the new document before replacing the file. Invalid keys and values MUST leave the prior file unchanged. It MUST write through a temporary file and rename it into place.

Workspace trust MUST persist as the top-level `mcp_trusted_workspaces` list of workspace paths in `settings.toml`. Only `WriteSettings` and `TrustWorkspace` write this key. An empty list means no workspace is trusted. Every entry MUST be a nonempty workspace path. `TrustWorkspace` MUST store the workspace it adds in canonical form (`filepath.Abs` plus `EvalSymlinks`, falling back to `Clean`). `dal` MUST reject the key as dalgona-only.

```toml
mcp_trusted_workspaces = ["/work/repo"]
```
