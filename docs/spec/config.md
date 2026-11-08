# Configuration

The program MUST read `config.toml` and then `settings.toml` from its product home. Settings MUST override config values. Command flags MUST override both files. Defaults MUST apply when no layer sets a value.

`dal` uses `$DAL_HOME` or `~/.dal`. `dalgona` uses `$DALGONA_HOME` or `~/.dalgona`. The loader MUST expose the resolved home. The two homes MUST remain separate.

The loader MUST reject unknown keys. The error MUST report the file and its 1-based line. `dal` MUST reject `tool_mode`, `runner`, `remote`, `runners`, `mcp_servers`, `discovery`, `eval`, `herdr`, and `web` as dalgona-only settings. Presence MUST trigger this rejection, including an empty table or a false value.

The default approval mode is `auto`. The default sandbox is `workspace-write`. The default tool mode is `normal`. The default runner is `local`. The default reasoning level is `default`.

The approval mode MUST be the top-level scalar `approval`. Its allowed values are `auto`, `yolo`, and `ask`. The decider settings MUST use `[approval_decider]`:

```toml
approval = "auto"

[approval_decider]
decider = "decisions:typesafe/jev-1.14"
threshold = 0.9
```

This shape resolves a conflict in the original plan. TOML cannot hold `approval = "auto"` and `[approval]` in one document. The plan therefore intended one scalar and one table, but used one key for both. `[approval_decider]` preserves both behaviors without renaming the approval mode. `approval_cfg` and `[approval]` are not valid configuration keys.

The decider MUST start with `decisions:` or `llm:`. The threshold MUST be in `(0, 1]`. Validation errors MUST name `[approval_decider] decider` or `[approval_decider] threshold`.

The loader MUST validate approval, sandbox, reasoning, tool mode, durations, and runner options. Durations MUST use Go duration syntax. Invalid durations MUST fail loading. A sandbox0 runner MUST use a template and MUST NOT set an image.

Edit styles MUST be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, or strict. A scalar style MUST apply to every model. A style table MUST contain `default`. Model patterns MUST match the full model name without regard to case. `*` matches any run and `?` matches one character. The first row in TOML source order wins.

A custom keyword name MUST match `^[a-z][a-z0-9_-]{1,31}$`. A custom keyword MUST set exactly one of `notice` or `text`. A notice path MUST be relative to the product home and MUST name an existing regular file. A built-in keyword override MAY set only selected fields, including `enabled = false`.

`WriteSetting` MUST update only `settings.toml`. It MUST preserve `config.toml`. It MUST preserve the source order of existing `edit_style` rows. It MUST decode and validate the new document before replacing the file. Invalid keys and values MUST leave the prior file unchanged. It MUST write through a temporary file and rename it into place.
