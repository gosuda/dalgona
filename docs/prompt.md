# The system prompt

The system prompt is the text dal sends to the model before your first message. Dal builds it from parts. Plugins, skills, and rules add to it.

Use this page to see what the model reads and which parts you can extend.

## The parts

The prompt assembles in this order: identity, D2, tools, skills, rules, rulebook, plugins, environment, instructions, fallback, docs line.

| part | what it holds |
|---|---|
| identity | one paragraph: `You are dal <version>, a coding agent. Work in the current directory. Read files before you edit them. Keep responses short and factual. Finish the whole task before you report.` |
| D2 | the line `D2 is preferred for diagrams.` |
| tools | a `<tools>` block with one line for each tool: `- <name>: <description>` |
| skills | the skill list |
| rules and rulebook | the rules that always apply, then the rulebook text |
| plugins | the `prompt` text of each plugin that sets one |
| docs line | `Read dal://index with the read tool when the user asks about dal itself.` |

The tools block is left out when no tool is listed. This page lists the parts. Read it from the terminal with `dalgon docs dal://prompt`.

## Add text from a plugin

A plugin adds one fixed section to the plugins part. Set `prompt` in `plugin.star`. The model reads the text as written.

```starlark
load("@dal/v1", "dal")

plugin = dal.plugin(
    name = "style",
    version = "0.1.0",
    prompt = "Prefer small, reviewable changes.",
)
```

[Plugins](plugins.md#add-skills-rules-and-prompt-text) shows `prompt`, `skills`, and `rules` in one plugin.

## Skills and letters

Skill descriptions appear in the prompt or as a letter. Letters carry long stable text as images. You can read the exact text at `letter://<id>`. A description that cannot be drawn falls back to text.

## AGENTS.md and SYSTEM.md

The prompt has a slot for user instructions. The prompt code can render that slot from `AGENTS.md` files and a `SYSTEM.md` file. It labels each `AGENTS.md` as `# Instructions from <path>`, root first. It adds the `SYSTEM.md` text from the data root with no label. Instructions are context for the model. They never act as a permission boundary.

The session driver does not pass `AGENTS.md` or `SYSTEM.md` text to this slot yet. A file you create for them does not reach the model.

## Font license

This product bundles GNU Unifont 18.0.01 glyph data for letter images. Upstream offers the font under the SIL Open Font License, Version 1.1, and GNU GPL version 2 or later with the GNU Font Embedding Exception. This distribution uses the SIL Open Font License, Version 1.1. Read the full bundled terms at dal://license under the appendix “Third-party font: GNU Unifont 18.0.01”. Upstream: https://unifoundry.com/unifont/.

## Next

- [Rules](rules.md) explains always rules, rulebook files, and stream rules.
- [Plugins](plugins.md#add-skills-rules-and-prompt-text) shows how a plugin adds skills, rules, and prompt text.
- [Sessions, history, and compaction](sessions.md) explains compaction, which starts at 85 percent of the context window.
