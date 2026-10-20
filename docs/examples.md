# Example plugins for dal

Ports of pi extensions as dal Starlark plugins, plus original dal plugins with no pi counterpart. Each port keeps its behavior. pi-specific UI becomes data. Copy the example nearest to your extension, then edit it. Each example is one directory with a `plugin.star` and a `README.md`. The MIT license for the pi ports is under [pi's license](#pis-license) below the table.

## Install one example

Run these commands from the repository root of a source checkout. Replace `hello` with any name from the table.

```sh
d="${XDG_DATA_HOME:-$HOME/.local/share}/dal/plugins/hello"; mkdir -p "$d"
cp -r examples/plugins/hello/. "$d/"
```

Add the name to `plugins` in `dal.toml`:

```toml
plugins = ["hello"]
```

Start dal. A plugin runs with your permissions, so read it before you install it. Three examples take settings: `env-read`, `mcp`, and `permission-gate`. The Settings section of each page lists them. See [Plugins](plugins.md) for every construct that an example uses.

## The examples

The `port` column links to the source in a checkout. Read a page in the manual with `dalgon docs dal://examples/<name>`. It holds the README and every file of the example.

| port | pi origin | what it shows | manual page |
|---|---|---|---|
| [hello](../examples/plugins/hello/plugin.star) | pi `examples/extensions/hello.ts` | the smallest shape | `dal://examples/hello` |
| [todo](../examples/plugins/todo/plugin.star) | pi `todo.ts` | a tool with state | `dal://examples/todo` |
| [subagent](../examples/plugins/subagent/plugin.star) | pi `subagent/index.ts` | bounded child sessions | `dal://examples/subagent` |
| [ask](../examples/plugins/ask/plugin.star) | pi `questionnaire.ts` | questions to the user | `dal://examples/ask` |
| [mcp](../examples/plugins/mcp/plugin.star) | the shape of pi-mcp-adapter | MCP calls through `mcp.call` and plugin settings | `dal://examples/mcp` |
| [permission-gate](../examples/plugins/permission-gate/plugin.star) | pi `permission-gate.ts` | a `tool_call` hook and settings | `dal://examples/permission-gate` |
| [plan-mode](../examples/plugins/plan-mode/plugin.star) | the shape of pi-plan-mode | a command that toggles saved state | `dal://examples/plan-mode` |
| [git-checkpoint](../examples/plugins/git-checkpoint/plugin.star) | pi `git-checkpoint.ts` | listing and restoring checkpoints through `tools.exec` | `dal://examples/git-checkpoint` |
| [handoff](../examples/plugins/handoff/plugin.star) | pi `handoff.ts` | child session notes saved to state | `dal://examples/handoff` |
| [skill-pack](../examples/plugins/skill-pack/plugin.star) | pi `dynamic-resources` | skills only | `dal://examples/skill-pack` |
| [fetch](../examples/plugins/fetch/plugin.star) | original to dal | HTTP fetch through `net.fetch`, with `dal.err` failures | `dal://examples/fetch` |
| [steer](../examples/plugins/steer/plugin.star) | original to dal | steering or waking the turn through `turn.steer` and `turn.wake` | `dal://examples/steer` |
| [review-rules](../examples/plugins/review-rules/plugin.star) | original to dal | stream and always-apply rules through `dal.rule` | `dal://examples/review-rules` |
| [locate](../examples/plugins/locate/plugin.star) | original to dal | file search and read through `tools.search` and `tools.read` | `dal://examples/locate` |
| [session-log](../examples/plugins/session-log/plugin.star) | original to dal | `session_start` and `turn_end` hooks with state | `dal://examples/session-log` |
| [env-read](../examples/plugins/env-read/plugin.star) | original to dal | one allowed variable through `env.read` and plugin settings | `dal://examples/env-read` |

---

## pi's license

The pi ports carry pi's MIT license. This is the notice that goes with them.

This product includes Starlark translations of examples of pi (https://github.com/earendil-works/pi), Copyright (c) 2025 Mario Zechner, used under the MIT License.

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## Next

- [Plugins](plugins.md): look up each construct that an example uses.
- [Port a pi extension to a dal plugin](convert-pi.md): translate your own pi extension.
