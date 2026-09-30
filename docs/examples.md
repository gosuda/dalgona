# Example plugins ported from pi

Ports of pi extensions as dal Starlark plugins. Each port keeps its behavior; pi-specific UI becomes data.

| port | pi origin | what it shows |
|---|---|---|
| hello | pi `examples/extensions/hello.ts` | the smallest shape |
| todo | pi `todo.ts` | a tool with state |
| subagent | pi `subagent/index.ts` | child sessions |
| ask | pi `questionnaire.ts` | questions to the user |
| mcp | the shape of pi-mcp-adapter | MCP calls through `mcp.call` and plugin settings |
| permission-gate | pi `permission-gate.ts` | a `tool_call` hook and settings |
| plan-mode | the shape of pi-plan-mode | a command that toggles saved state |
| git-checkpoint | pi `git-checkpoint.ts` | checkpoints through `tools.exec` |
| handoff | pi `handoff.ts` | child session notes |
| skill-pack | pi `dynamic-resources` | skills only |

## pi's license

This product includes Starlark translations of examples of pi (https://github.com/earendil-works/pi), Copyright (c) 2025 Mario Zechner, used under the MIT License.

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
