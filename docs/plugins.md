# Plugins: tools, commands, hooks, skills, and rules

A plugin is Starlark that adds tools, slash commands, hooks, skills, rules, and prompt text to dal. It is one directory holding one file, `plugin.star`. That file publishes one value named `plugin`. Nothing else registers. A tool you build but do not attach to `plugin` does not exist. A plugin runs with your permissions, so read it before you install it.

## Write a first plugin

You write one file, turn it on, grant it one permission, and call it. The plugin finds `TODO` comments in your code.

Make a directory under the data root and write `plugin.star` into it. The directory name is the plugin name.

```sh
d="${XDG_DATA_HOME:-$HOME/.local/share}/dal/plugins/quality"
mkdir -p "$d"
cat > "$d/plugin.star" <<'EOF'
load("@dal/v1", "dal")

def find_todos(ctx, args):
    page = ctx.tools.search("TODO", path = args.path)
    return {
        "matches": [
            {"path": hit.path, "line": hit.line, "text": hit.text}
            for hit in page.matches[:args.limit]
        ],
        "truncated": page.truncated or len(page.matches) > args.limit,
    }

todos = dal.tool(
    description = "Find TODO comments under a path.",
    input = dal.schema(
        path = dal.string(default = "."),
        limit = dal.integer(default = 20, min = 1, max = 200),
    ),
    uses = ["tools.search"],
    run = find_todos,
)

plugin = dal.plugin(
    name = "quality",
    version = "0.1.0",
    tools = {"todos": todos},
    commands = {
        "todos": dal.command(tool = todos, positional = ["path"]),
    },
)
EOF
```

On Windows, make the directory with the PowerShell command in [Config and data roots](install.md#config-and-data-roots). Then save the same Starlark text as `plugin.star` in it.

Turn it on. Add its name to the `plugins` key in `${XDG_CONFIG_HOME:-$HOME/.config}/dal/dal.toml`. Put the key above any `[table]` header. Create the file if it does not exist.

```toml
plugins = ["quality"]
```

Check that dal loads the plugin. A load error stops the command and names `path:line:col`.

```sh
dalgon plugin list
```

The handler calls `tools.search`, which reads files. So the plugin asks for the `fs.read` service. No grant exists yet:

```text
quality (user): services: fs.read; persistent grant: not granted
```

Grant the service. Run the command from the workspace where you will use the plugin.

```sh
dalgon plugin grant quality
```

```text
Granted "quality" (user): fs.read.
```

Run `dalgon plugin list` again. The row now ends with `persistent grant: granted`. A grant covers one workspace and one version of the plugin code. When you edit `plugin.star`, grant again.

Skip the grant, and dal asks on the first call in the terminal. A headless run cannot ask, so dal denies the call. The denial reads `denied: plugin "quality" has no grant for the "fs.read" service; approve the plugin's grant request when it asks, then try again`.

Run dal. The model can now call the tool. You can type `/quality:todos src`. The result holds `matches`, a list of records with `path`, `line`, and `text`, and a `truncated` flag.

Four things carry the whole design:

- `load("@dal/v1", "dal")` appears once. It is the only import a plugin may use.
- A handler is `run(ctx, args)`. dal checks the arguments against `input` and fills defaults. Only then does it call the handler.
- `uses` lists every operation the handler may request. dal denies a call to any other operation and ends the handler.
- `plugin` is the only value a plugin publishes. Its `name` must equal the directory name.

## Fill in the plugin value

`dal.plugin` takes keywords only. `name` and `version` are required.

| keyword | value | default |
|---|---|---|
| `name` | the directory name: lowercase letters, digits, `_`, `-`, starting with a letter, at most 64 characters | required |
| `version` | a SemVer string such as `"0.1.0"` | required |
| `config` | a `dal.schema` for the plugin's settings | none |
| `state_version` | a positive integer naming the shape of saved state | `1` |
| `tools` | a map from a local key to a `dal.tool` | empty |
| `commands` | a map from a key to a `dal.command` | empty |
| `hooks` | a list of `dal.on` | empty |
| `skills` | a map from a name to a `dal.skill` | empty |
| `rules` | a map from a name to a `dal.rule` | empty |
| `prompt` | one static prompt section, or `None` | `None` |
| `models` | a map from a local key to a `dal.model` | empty |

A tool key `todos` in plugin `quality` has the identity `quality.todos`. One `dal.tool` can have at most one key. A tool with no key still serves a command. dal keeps saved state apart for each plugin and each `state_version`. Changing the version does not migrate old state.

## Define a tool

`dal.tool` takes keywords only.

| keyword | meaning | default |
|---|---|---|
| `description` | what the model reads to choose the tool | required |
| `input` | a `dal.schema` for the arguments | required |
| `run` | the handler, `run(ctx, args)` | required |
| `uses` | the operations the handler may request | `[]` |
| `output` | a `dal.schema` the returned value must satisfy | none |
| `visibility` | `"model"`, `"deferred"`, or `"eval_only"` | `"model"` |

`"model"` puts the tool in the model's ordinary tool list. `"deferred"` keeps the tool out of the list until the model first calls it by name. The call runs at once. The tool joins the list in the next request and in every later request. A request offers exactly one way to find deferred tools. On first-party Anthropic models from Opus, Sonnet, Fable, and Mythos 4.5 on, Anthropic's own tool search finds them. Each tool travels in the request, marked `defer_loading`. Every other model gets the `tool_search` tool. It lists matches and changes nothing. A plugin tool named `tool_search` replaces both. `"eval_only"` reaches the tool only through `eval`.

### Describe arguments with a schema

`dal.schema(**fields)` builds the schema. Each field is one of these constructors. Keywords are named. The first argument of `enum`, `list`, `optional`, and `nullable` is positional.

| constructor | accepts |
|---|---|
| `dal.string` | `min_len`, `max_len`, `default` |
| `dal.integer` | `min`, `max`, `default` |
| `dal.number` | `min`, `max`, `default` |
| `dal.boolean` | `default` |
| `dal.enum(values)` | a list of strings, `default` |
| `dal.list(item)` | `min_len`, `max_len`; `item` is a bare type |
| `dal.optional(type)` | the caller may leave the field out |
| `dal.nullable(type)` | the field is required but may be `None` |

A field is required unless it has a `default` or you wrap it in `optional`. When a caller leaves out an `optional` field, the handler reads `dal.MISSING`:

```starlark
def note(ctx, args):
    if args.tag == dal.MISSING:
        return "No tag given."
    return "Tag: " + args.tag
```

Field names are plain identifiers and never start with `_`. Bounds must not contradict each other. dal coerces nothing. The string `"5"` does not satisfy an integer field. A truthy value does not satisfy a boolean.

### Return a result

Return plain data. That data is the result. Three helpers say more:

- `dal.ok(value)` returns a success explicitly.
- `dal.err(code, message, details)` returns a failure the model can read. `details` is optional. The codes `failed`, `exit_nonzero`, `unavailable`, `conflict`, `busy`, `cancelled`, `observation_unavailable`, `invocation_mismatch`, and `indeterminate` belong to dal. `dal.err` refuses them.
- `dal.output(value = ..., view = ...)` separates the data from its display. A view is `{"type": "text", "text": "..."}`, `{"type": "table", "columns": [...], "rows": [[...]]}`, an intact view from `tools.read`, or a list of views. Table rows hold scalar values. A view changes what people see, never the status of the call.

```starlark
load("@dal/v1", "dal")

def count(ctx, args):
    page = ctx.tools.search(args.pattern, path = args.path)
    total = len(page.matches)
    return dal.output(
        value = {"count": total, "truncated": page.truncated},
        view = {
            "type": "table",
            "columns": ["pattern", "matches"],
            "rows": [[args.pattern, total]],
        },
    )

def branch(ctx, args):
    result = ctx.try_call(ctx.tools.exec, "git branch --show-current")
    if not result.ok:
        return dal.err("no_git", "Could not read the branch: " + result.error.message)
    return dal.ok(result.value)

count_tool = dal.tool(
    description = "Count matches for a pattern.",
    input = dal.schema(
        pattern = dal.string(min_len = 1),
        path = dal.string(default = "."),
    ),
    uses = ["tools.search"],
    run = count,
)

branch_tool = dal.tool(
    description = "Show the current git branch.",
    input = dal.schema(),
    uses = ["tools.exec"],
    run = branch,
)

plugin = dal.plugin(
    name = "report",
    version = "0.1.0",
    tools = {"count": count_tool, "branch": branch_tool},
)
```

A plain dictionary that happens to contain `ok = False` is still data. Only `dal.err` reports a failure.

## Reach the outside with ctx

`ctx.config` holds the validated settings. Every other field of `ctx` is a group of operations. Each call returns its value directly:

```starlark
view = ctx.tools.read("src/lib.rs")
```

`tools.read` returns a view with `path`, `header`, `truncated`, and `rows`. Each row has `line`, `text`, and `complete`. `tools.search` returns a page with `matches` and `truncated`. Each match has `path`, `line`, `text`, and a `source` read view. A handler cannot return a view itself. Return plain data. To display the view, pass it as the `view` of `dal.output`.

An operation that fails stops the handler with the error. To handle an expected failure yourself, pass the operation, not its result, to `ctx.try_call`. You get a result with `ok`, `value`, and `error`. `unwrap()` returns the value or raises the failure again. Denial, cancellation, and hard limits still stop the handler. `try_call` cannot catch them and never retries.

These are the operations. A handler may call only those in its `uses`.

| group | operations | purpose |
|---|---|---|
| `tools` | `read`, `search`, `patch`, `exec` | read files, search, edit files, run a command |
| `models` | `infer` | ask a model for a completion |
| `net` | `fetch` | make an HTTP request to an allowed destination |
| `ask` | `confirm`, `select`, `text` | ask the user a question |
| `state` | `read`, `write`, `delete` | keep data for this plugin and session |
| `agents` | `start`, `wait`, `cancel`, `list` | run child sessions |
| `jobs` | `start`, `wait`, `cancel`, `list`, `text` | run background jobs |
| `turn` | `cancel`, `steer`, `wake`, `is_idle` | steer the running turn or start one |
| `env` | `read` | read an allowed environment variable |
| `mcp` | `call` | call an MCP tool when a client is present |

Write the id as `group.operation`, for example `"state.write"`. Ids are exact. Use no wildcards and no duplicates. Use at most 64 ids per declaration. The positional argument of `tools.read` is `path`. The positional argument of `tools.search` is `pattern`. The positional argument of `tools.exec` is `command`. Every other argument is named. Call `ctx.describe("tools.exec")` to see the keywords of one operation.

`agents.start` does not accept a per-call `tools` list. If you pass one, the call fails with an argument error. If you omit `name`, the call starts an unnamed child. A given `name` must be unique among the sessions in the workspace. A taken name refuses the start. When policy or input stops a child, `agents.start` answers `refused` with a typed `reason`. The reasons are `max_depth` (`child sessions cannot start children here: agents.max_depth = <n>.`), `workspace_unresolved`, `workspace_outside_root`, and `model_unroutable`. Show the reason text to the user. A `cancelled` reply still means the child was cancelled.

`uses` asks. It does not grant. Each effect still needs a live grant and passes the approval ladder, so `tools.patch` and `tools.exec` ask before they run. `ask` operations must be in `uses` but need no grant. `turn.wake` refuses after 20 turns in a row that wake started with no user prompt. `models.forward` exists only for model handlers. Plugins cannot use it.

State is explicit and session-scoped. A read returns a record with `present`, `value` (when present), and `revision`. A write or delete must pass the `revision` from a read as `expected`. A stale revision fails with `conflict`. `ctx.try_call` can catch that. Keys match `[a-z][a-z0-9_.-]{0,63}`. An ephemeral session returns `unavailable` for state. The `todo` example keeps a list this way. Read it at `dal://examples/todo`.

A handler cannot call another plugin's tool. One plugin can share code between its handlers with plain functions, and two plugins can read the same file.

## Add a command

`dal.command(tool = ..., positional = [...], description = ...)` binds a slash command to a tool. The command runs that tool once, with the same schema and defaults a model call gets. Without `description`, the tool's description is the command's summary. The command name is `/<plugin>:<key>`. The key uses lowercase letters, digits, `_`, and `-`. It starts with a letter and has at most 64 characters. A name that clashes with a built-in command is an error that names both claimants.

`positional` lists scalar fields in order. A required field cannot follow an optional one. The rest take `--field=value`. For strings, `--field value` also works. Booleans take `--field` and `--no-field`. Lists and records take one JSON value: `--field=JSON`. `--` ends option parsing. Single and double quotes group a token. A backslash escapes the next character outside single quotes. Assigning one field twice is an error. Nothing expands. There are no globs, variables, or shell.

## Hook into the turn

`dal.on(event, run, uses = [...])` runs `run(ctx, event)` at one lifecycle event. The event value carries its payload as attributes, such as `event.tool`, and the methods that return a verdict.

| event | payload | the handler returns | may request |
|---|---|---|---|
| `session_start` | `session`, `workspace`, `resumed` | `None` | `state.read`, `state.write`, `state.delete` |
| `session_end` | `session`, `reason` | `None` | `state.read`, `state.write`, `state.delete` |
| `input` | `text` | `event.continue_()`, `event.transform(text)`, `event.handled()` | nothing |
| `before_turn` | `turn`, `text` | `event.continue_()`, `event.append(text)` | nothing |
| `before_request` | `turn`, `round`, `model`, `caps`, `params` | `event.continue_()`, `event.params(...)` | nothing |
| `tool_call` | `turn`, `call`, `tool`, `args` | `event.allow()`, `event.block(reason)`, `event.rewrite(args)` | nothing |
| `tool_result` | `turn`, `call`, `tool`, `ok`, `preview` | `None` | `state.read`, `state.write`, `state.delete` |
| `turn_end` | `turn`, `stop` | `None` | `state.read`, `state.write`, `state.delete` |
| `settled` | `turn` | `None` | `state.read` |

The four hooks that decide something (`input`, `before_turn`, `before_request`, `tool_call`) are pure. They see only the event, the plugin's config, and fixed metadata. They may declare no `uses`. Naming an operation outside a hook's column is a load error. `event.params` takes `thinking`, `effort`, `temperature`, and `max_output_tokens`. dal clamps them to the model's limits. `preview` holds at most 4 KiB of the result.

A verdict is valid only for the event that made it. Returning `None`, a dictionary, or another event's verdict from a deciding hook is a failure, not an implicit allow. A failed `tool_call` hook blocks the call. A failed `input`, `before_turn`, or `before_request` hook changes nothing. dal records a failed observer and leaves the result unchanged.

Hooks run in one fixed order: built-ins, then bundled plugins by name, then your plugins by name. Inside one plugin, hooks run in list order. A `rewrite` feeds the next hook. The first `block` or `handled` ends the chain. One exception applies: on `input`, a built-in `handled` still lets later origins run. `before_turn` texts join with a blank line.

```starlark
load("@dal/v1", "dal")

def guard_patch(ctx, event):
    if event.tool == "patch" and ctx.config.read_only:
        return event.block("Patch is disabled by this plugin's policy.")
    return event.allow()

def remember(ctx, event):
    record = ctx.state.read(key = "last_tool")
    ctx.state.write(key = "last_tool", value = event.tool, expected = record.revision)

plugin = dal.plugin(
    name = "policy",
    version = "0.1.0",
    config = dal.schema(read_only = dal.boolean(default = False)),
    hooks = [
        dal.on("tool_call", guard_patch),
        dal.on("tool_result", remember, uses = ["state.read", "state.write"]),
    ],
)
```

This hook blocks `patch` by name. It does not stop `exec` from writing a file. A hook is policy, not a boundary. The boundary is the approval ladder and the sandbox.

## Read settings

`config = dal.schema(...)` declares the settings. Values come from the `[plugin.<name>]` table in `dal.toml`:

```toml
[plugin.policy]
read_only = true
```

The handler reads `ctx.config.read_only`. An unknown key, a wrong type, or an invalid default fails at load, before any handler runs. Read an optional setting with `getattr(ctx.config, "name", dal.MISSING)`. A plugin gets settings only this way.

## Add skills, rules, and prompt text

`dal.skill(description, path)` publishes a skill. The path is relative to the plugin directory, so dal reads `skills/review/SKILL.md` when the plugin loads. `letter2image = True` turns on letter handling for the skill. The [system prompt](prompt.md) page describes letter handling. Skills collide by name. A clashing record is not published. An ambiguous short name refuses to resolve. A clash without an explicit replacement is refused.

A user plugin can take over one bundled skill or rule by naming it explicitly. The mechanism is shared in dal; in dalgona the bundled owners include `skills` and `quality`. Only `skill` and `rule` records are replaceable. The extension name itself is never replaceable.

`dal.skill(..., replace = True, battery = "<bundled extension>")` and `dal.rule(..., replace = True, battery = "<bundled extension>")` carry the claim as one inseparable pair. Omitting the pair, or writing `replace = False`, keeps the default refusal of a clashing record. `replace` is a boolean and `battery` is a string; an explicit `None` for either is a wrong-type refusal at the declaring call, not an omission. Writing `battery` without `replace = True`, or `replace = True` without `battery`, is a strict input refusal at the call that names both keywords.

The owner is never implied and must name the bundled extension exactly. A name no bundled extension ships, a bundled extension that does not register the record, a built-in extension, or a user plugin each refuses the load. Naming the wrong bundled extension refuses even when another bundled extension registers the record. A record another user plugin already replaced refuses as well. A plugin named like a battery stays rejected even with the pair.

A replacement moves one record and nothing else. The replacing extension keeps its own plain user identity and inherits no extension name, service request, tool, prompt section, status kind, or grant from the owner.

A skill that replaces the bundled `find-anything` skill owned by `skills`:

```starlark
load("@dal/v1", "dal")

plugin = dal.plugin(
    name = "mine",
    version = "0.1.0",
    skills = {
        "find-anything": dal.skill(
            description = "Mine replaces the bundled finder.",
            path = "skills/find-anything/SKILL.md",
            replace = True,
            battery = "skills",
        ),
    },
)
```

A rule that replaces the bundled `fabricated-unavailable-tool-call` rule owned by `quality`:

```starlark
load("@dal/v1", "dal")

plugin = dal.plugin(
    name = "mine",
    version = "0.1.0",
    rules = {
        "fabricated-unavailable-tool-call": dal.rule(
            pattern = "\\bfoo\\b",
            text = "Mine replaces the bundled detector.",
            replace = True,
            battery = "quality",
        ),
    },
)
```

Reload rebuilds the declared owners. Removing the claim and renaming or deleting the user record restores the bundled original. Keeping the colliding record without the claim refuses the reload and the previous plugin set stays live.

`dal.rule(text, pattern, judge, always_apply, scope, interrupt_mode, repeat_mode, repeat_gap)` publishes a rule. Every argument is a keyword. Only `text` is required. A rule with a `pattern` watches the model's output. The `pattern` is a regular expression. On a match, dal tells the model the `text`. A rule with `always_apply = True` and no `pattern` applies every turn. A rule needs a pattern unless it always applies. A rule with neither fails at the call. `judge` is optional instruction text for judged rules.

The last four arguments narrow where and how often a rule fires. Leave one out to keep the `[rules]` default.

| Argument | Values |
| --- | --- |
| `scope` | a list of `text`, `thinking`, `tool`, or `tool:<name>`, such as `["tool:exec"]` |
| `interrupt_mode` | `always`, `prose-only`, `tool-only`, or `never` |
| `repeat_mode` | `once` or `after-gap` |
| `repeat_gap` | a whole number from 1 to 1000 |

A value outside these lists stops the plugin from loading. The error names the argument and the values to use. Rules, the `[rules]` settings, and the judged-rule gate are on the [Rules](rules.md) page.

`prompt` is one fixed section of the system prompt.

```starlark
load("@dal/v1", "dal")

plugin = dal.plugin(
    name = "style",
    version = "0.1.0",
    prompt = "Prefer small, reviewable changes.",
    skills = {
        "review": dal.skill(
            description = "Review a diff for missing tests. Use when the user asks for a review.",
            path = "skills/review/SKILL.md",
        ),
    },
    rules = {
        "no-todo": dal.rule(
            pattern = "\\bTODO\\b",
            text = "Finish the TODO now, or say why it stays.",
            scope = ["text", "tool:patch"],
            repeat_mode = "after-gap",
            repeat_gap = 10,
        ),
        "small-diffs": dal.rule(
            text = "Keep each change small enough to review at a glance.",
            always_apply = True,
        ),
    },
)
```

## Call a plugin tool from eval

An `eval` cell can call a published tool as `tools["<plugin>.<key>"]`. The cell needs both the tool's id and the operations that tool declares:

```starlark
tools["quality.todos"](path = "src")
```

That cell needs `tools.quality.todos` and `tools.search` in its operations. [Tools, modes, and approval](tools.md#the-eval-tool) explains how a cell gets them.

## Load, reload, and errors

Load has no compile step. A broken plugin stops startup and names `path:line:col`. Load is all or nothing. One bad plugin stops the whole set. Type `/reload` to re-evaluate the plugins. It prints counts, for example `Reloaded 2 plugins: 3 tools. New turns use them. A running turn keeps the plugins it started with.` On failure, it prints `reload failed: <rendered error>` on one line and `the previous plugin set stays live` on the next.

Plugins come from the data root only, never from the workspace. A plugin that loads in dalgon loads in dalgona. See `dalgona://batteries` for the batteries product.

## Declare scripted models

A plugin can declare `models` with `dal.model(id = ..., caps = ..., run = ..., uses = [...])`. A scripted model that runs where no script context exists fails with `script context is unavailable for this model`. To call a model from a tool, request `models.infer`.

## Next

- [Example plugins for dal](examples.md) lists 16 working plugins you can copy.
- [Port a pi extension to a dal plugin](convert-pi.md) converts an existing pi extension.
- [Tools, modes, and approval](tools.md#the-eval-tool) shows how an `eval` cell calls your tools.
