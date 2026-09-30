# Port a pi extension to a dal plugin

## Before you start

A pi extension is TypeScript that pi loads with jiti; a dal plugin is Starlark that dal evaluates at load. Both run with your permissions; neither is a sandbox. A port is a redesign more often than a translation. Read the `plugins` page first: it defines every construct named below.

## Get the source

From npm, `npm pack <package>` prints a file name, then `tar -xzf <file>.tgz` unpacks it into `./package`. From git, `git clone <url> package`. Pi's own examples live in `packages/coding-agent/examples/extensions/` of the pi repository.

## Take inventory

```sh
grep -RnoE '\b(pi|ctx)\.[A-Za-z_$][A-Za-z0-9_$]*' package --include='*.ts'
grep -RhoE "from '[^.][^']*'" package --include='*.ts' | sort -u
```

## See one tool ported

A pi tool of this shape:

```ts
pi.registerTool({
  name: "greet",
  description: "Greet someone by name.",
  parameters: Type.Object({ name: Type.String() }),
  async execute(id, params) {
    return { content: [{ type: "text", text: `Hello, ${params.name}.` }] };
  },
});
```

becomes this plugin, saved as `greet/plugin.star`:

```starlark
load("@dal/v1", "dal")

def greet(ctx, args):
    return "Hello, " + args.name + "."

greet_tool = dal.tool(
    description = "Greet someone by name.",
    input = dal.schema(name = dal.string()),
    run = greet,
)

plugin = dal.plugin(
    name = "greet",
    version = "0.1.0",
    tools = {"greet": greet_tool},
)
```

The tool name moves from `name` to its key in `tools`. The schema moves from `parameters` to `input`. The result moves from a content list to the returned value.

## Map each call

| pi | dal | Shown in |
|---|---|---|
| `export default function (pi: ExtensionAPI)` | `plugin = dal.plugin(name = "...", version = "...")` in `plugin.star`, after one `load("@dal/v1", "dal")`; `name` matches the directory | every port |
| `pi.registerTool({name, description, parameters, execute})` | `dal.tool(description, input, run, uses)`, published as `tools = {"<name>": tool}`; `uses` lists the operations the handler may request | todo, subagent |
| TypeBox `Type.Object`, string enums | `dal.schema(...)` with `dal.string`, `dal.integer`, `dal.enum([...])`, `dal.list`; `dal.optional` for a field a caller may omit | todo, ask |
| `execute(id, params, signal, onUpdate, ctx)` | `run(ctx, args)`; dal cancels the call and discards the result; progress text is not ported | subagent |
| result `{content, details}` | the returned value; `dal.output(value = ..., view = ...)` when people should see a table or text apart from the data; `details` has no counterpart | todo (the returned value) |
| `throw` inside `execute` | `return dal.err(code, message)` for a failure the model should read; a failed operation stops the handler by itself | none yet |
| `renderCall`, `renderResult` | the result and its `view`; plugin UI is data | todo |
| `pi.registerCommand(name, {handler})` | `dal.command(tool = tool, positional = [...])` under `commands`; the command runs a tool and is typed as `/<plugin>:<key>` | todo, git-checkpoint |
| `ctx.ui.select`, `confirm`, `input` | `ctx.ask.select`, `ctx.ask.confirm`, `ctx.ask.text`, with the `ask.*` ids in `uses` | ask |
| `pi.exec(cmd, args, {cwd, timeout})` | `ctx.tools.exec("cmd args")` with `tools.exec` in `uses`; the approval ladder applies; a non-zero exit stops the handler unless you call `ctx.try_call(ctx.tools.exec, "...")`; `ctx.describe("tools.exec")` lists the other keywords | git-checkpoint |
| `fetch`, npm HTTP clients | `ctx.net.fetch(...)` with `net.fetch` in `uses` | none yet |
| `pi.sendUserMessage(text)` | `ctx.turn.steer(...)` with `turn.steer` in `uses`; `ctx.turn.wake` starts a turn when dal is idle | none yet |
| state in `details`, `pi.appendEntry` | `ctx.state.read(key = ...)` and `ctx.state.write(key = ..., value = ..., expected = revision)`, with `state.read` and `state.write` in `uses` | todo, plan-mode, handoff |
| `before_agent_start` adds to `systemPrompt` | `prompt = "..."` on the plugin for fixed text; a `before_turn` hook that returns `event.append(text)` for per-turn text; the hook cannot read state | hello (`prompt`) |
| `pi.on("tool_call")` | `dal.on("tool_call", handler)`; the handler returns `event.allow()`, `event.block(reason)`, or `event.rewrite(args)` | permission-gate |
| other `pi.on` events | `dal.on` with `input`, `before_turn`, `before_request`, `tool_result`, `turn_end`, `session_start`, `session_end`, or `settled`; each event fixes its verdict methods and the operations it may request | none yet |
| `pi.on("user_bash")`, whole-operation swap | does not port; no hook can hand over execution | (not-port table) |
| `pi.registerProvider` | does not port; providers are dal core and aliases live in config | (not-port table) |
| `ctx.ui.custom`, widgets, status, editor | does not port; plugin UI is data | (not-port table) |
| `ctx.modelRegistry` nested model call | `ctx.models.infer` with `models.infer` in `uses`, or a child session through `ctx.agents.start` and `ctx.agents.wait` | subagent, handoff (child sessions) |
| `pi.registerFlag`, `registerShortcut` settings | `config = dal.schema(...)` on the plugin and a `[plugin.<name>]` table in `dal.toml`; the handler reads `ctx.config` | permission-gate, mcp |
| `pi.events` plugin bus | does not port; a handler cannot call another plugin's tool | (not-port table) |
| stdio MCP servers | `ctx.mcp.call` with `mcp.call` in `uses`, where dal has an MCP client (dalgona ships one); an external bridge program elsewhere | mcp |
| project `.pi/extensions` | never loads; plugins come from the data root only | (not-port table) |
| TypeScript or npm at run time | an external program through `ctx.tools.exec`, or a port of the logic | (not-port table) |
| `resources_discover` dynamic skills | `skills = {"<name>": dal.skill(description = ..., path = "skills/<name>/SKILL.md")}`, fixed at load; no dynamic discovery | skill-pack |

## Decide

If the purpose of the extension is in the not-port table, do not port it.

| pi shape | why it does not port | what dal or dalgona offers instead |
|---|---|---|
| `interactive-shell` | needs user takeover of the terminal; `tools.exec` waits for the child to exit and captures its streams; `ask` is one modal question | none today; the door can widen |
| `status-line` | needs a persistent status or footer surface; plugin UI is data, and the only widget is `ask` | none today; the door can widen |
| `pi.on("user_bash")` swap | no hook can hand over execution | run the program as a tool through `ctx.tools.exec` |
| `pi.registerProvider` | providers are dal core; a plugin cannot declare `models` in this release | config aliases |
| `ctx.ui.custom`, widgets, status, editor | plugin UI is data | the ask surface |
| `pi.events` bus | a handler cannot call another plugin's tool | one plugin, or a file that both read |
| project `.pi/extensions` | plugins come from the data root only | install into the data root |
| TypeScript or npm at run time | nothing runs TypeScript at load | an external program through `ctx.tools.exec`, or a port of the logic |
| web fetch, MCP client as a product | dalgona's batteries own them | dalgona's `web` and `mcp` batteries; in dal the `net.fetch` operation |

## Create the plugin directory

```sh
d="${XDG_DATA_HOME:-$HOME/.local/share}/dal/plugins/<name>"; mkdir -p "$d"
```

On Windows the data root follows the `install` page.

## Start from the nearest example

A tool with state, `todo`; child sessions, `subagent`; questions to the user, `ask`; an MCP call, `mcp`; a hook, `permission-gate`; skills only, `skill-pack`; the smallest shape, `hello`. The page `dal://examples/<name>` holds every file; from a source checkout, `cp -r examples/plugins/<name>/. "$d/"`. Then set `name` and rename the tools and commands.

## Translate

Translate one registration at a time with the map. The worked pair: pi `examples/extensions/todo.ts:105-297` beside the `todo` handler of `examples/plugins/todo/plugin.star`.

## Turn it on

Add the name to the plugins key in `${XDG_CONFIG_HOME:-$HOME/.config}/dal/dal.toml`:

```toml
plugins = ["<name>"]
```

Run dal. A load error stops startup and names `path:line:col`. Then run one print-mode turn that uses the new tool, with the CLI print-mode flag.

## Change and reload

Edit, then type `/reload`. Success prints counts, for example `Reloaded 2 plugins: 3 tools. New turns use them. A running turn keeps the plugins it started with.` Failure prints `reload failed: <rendered error>` on one line and `the previous plugin set stays live` on the next. With no plugins configured it prints `No plugins to reload: the plugins key in dal.toml is empty.`

## Check it in dalgona

Copy the directory to the dalgona data root, add the name to dalgona's config, and run `dalgona`. A plugin that loads in dalgon loads in dalgona.

## Write README.md

Write `README.md` in the plugin directory with sections `# <name>`, `Ported from`, `What it shows`, `What differs from pi`, `Install`, `Settings`, at most 60 lines. When you translate pi code, keep pi's MIT notice; `dal://examples` holds the text.

## Let dal port it

Type: `Port the pi extension in ./package to a dal plugin named <name>. Read dal://convert-pi and follow its steps. Write the plugin under the dal data directory, and tell me what did not port.` The model writes into the data root, so the approval ladder asks first. The `port-pi-extension` skill of `skill-pack` routes the model here.
