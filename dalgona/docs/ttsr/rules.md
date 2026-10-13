# rules

Dalgona ships seven bundled rule sets plus the compiled `detectors`
set: 23 rules in total. The `[rule_sets] enabled` list selects sets;
an unknown name fails startup, and duplicate entries are idempotent.
Every loaded rule's exact source is readable at `rule://<name>`.

## Origins

A rule's origin names where it comes from: a user file path under
`<data-root>/rules/`, a project path relative to the workspace root
(`<workspace>/.dal/rules/`), or `plugin:<extension>` for a rule that an
extension registers in code. The seven Markdown sets register through
the `ttsr-rules` extension, so their rules print `plugin:ttsr-rules`;
the set is the rule-name prefix, and each rule's compiled-in source is
the `<set>/rules/<rule>.md` file in its set table. The `detectors`
text rule registers through the `quality` extension (`plugin:quality`);
the three detector lanes are compiled code, not rules in the rule
table, and print no origin. A `rule_fired` journal record of a rule
registered in code carries its label in `origin`.

## Set inventory

### steer

| rule | source file | scope | interrupt | repeat |
|---|---|---|---|---|
| steer-no-apologies | steer/rules/steer-no-apologies.md | text | prose-only | after-gap 10 |
| steer-no-meta | steer/rules/steer-no-meta.md | text | prose-only | after-gap 10 |
| steer-no-restate | steer/rules/steer-no-restate.md | text | prose-only | after-gap 10 |

#### steer-no-apologies

condition: `(?i)\b(?:i apologize|my apologies|sorry for (?:the )?(?:confusion|the mistake)|you(?:'re| are) (?:absolutely )?right)\b`

---
description: Keeps the reply moving when the model apologizes or agrees.
condition: '(?i)\b(?:i apologize|my apologies|sorry for (?:the )?(?:confusion|the mistake)|you(?:''re| are) (?:absolutely )?right)\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Do not apologize and do not confirm the user was right. State the next concrete step and take it.

#### steer-no-meta

condition: `(?i)(?:\bas an ai (?:language )?model\b)|(?:\bi do not have (?:access|the ability) to\b)`

---
description: Fires on assistant-protocol meta talk that serves no task.
condition:
  - '(?i)\bas an ai (?:language )?model\b'
  - '(?i)\bi do not have (?:access|the ability) to\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Answer as the coding agent. Use the tools you have and continue the task.

#### steer-no-restate

condition: `(?i)\blet me (?:restate|repeat|recap|summarize) (?:the |your )(?:task|request|question|instructions)\b`

---
description: Fires when the model restates the request instead of working.
condition: '(?i)\blet me (?:restate|repeat|recap|summarize) (?:the |your )(?:task|request|question|instructions)\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Do not restate the request. Start or continue the work.

### compact

| rule | source file | scope | interrupt | repeat |
|---|---|---|---|---|
| compact-cite-files | compact/rules/compact-cite-files.md | text | prose-only | after-gap 10 |
| compact-no-context-loss | compact/rules/compact-no-context-loss.md | text | prose-only | after-gap 5 |
| compact-state-on-disk | compact/rules/compact-state-on-disk.md | always applied | - | - |

#### compact-cite-files

condition: `(?i)\bas (?:mentioned|said|discussed|described) earlier\b`

---
description: Fires when the model cites the conversation instead of a file.
condition: '(?i)\bas (?:mentioned|said|discussed|described) earlier\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Do not point to earlier conversation. Cite the file and line that hold the fact.

#### compact-no-context-loss

condition: `(?i)\b(?:i (?:do not|cannot) (?:recall|remember)|i (?:lost|no longer have) (?:access to )?(?:the )?(?:earlier|previous) (?:context|conversation))\b`

---
description: Fires when the model claims it lost earlier context.
condition: '(?i)\b(?:i (?:do not|cannot) (?:recall|remember)|i (?:lost|no longer have) (?:access to )?(?:the )?(?:earlier|previous) (?:context|conversation))\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 5
---
State lives in files, not memory. Read the todo list and the files you touched, then continue.

#### compact-state-on-disk

Always applied; no condition.

---
description: Durable state lives in the workspace, not in the conversation.
alwaysApply: true
---
Keep durable state in the workspace. The todo list and plan files hold progress; write a step to disk before you rely on it. The conversation is not storage.

### stop

| rule | source file | scope | interrupt | repeat |
|---|---|---|---|---|
| stop-act-dont-offer | stop/rules/stop-act-dont-offer.md | text | prose-only | after-gap 5 |
| stop-evidence-before-done | stop/rules/stop-evidence-before-done.md | text | prose-only | after-gap 5 |
| stop-finish-the-work | stop/rules/stop-finish-the-work.md | text | prose-only | after-gap 10 |

#### stop-act-dont-offer

condition: `(?i)\b(?:shall i|should i|do you want me to|would you like me to) (?:continue|proceed|fix|implement|run)\b`

---
description: Fires on reflexive offers to continue instead of acting.
condition: '(?i)\b(?:shall i|should i|do you want me to|would you like me to) (?:continue|proceed|fix|implement|run)\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 5
---
Act on the decided plan. Ask only for owner decisions: irreversible, destructive, safety-critical, or cross-cutting choices.

#### stop-evidence-before-done

condition: `(?i)\b(?:all (?:tests|checks) (?:pass|passed)|everything (?:is )?work(?:ing|s))\b`

---
description: Fires on completion claims that no run in this session backs.
condition: '(?i)\b(?:all (?:tests|checks) (?:pass|passed)|everything (?:is )?work(?:ing|s))\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 5
---
A completion claim needs evidence from this session. Run the project gates and quote their output before you claim done.

#### stop-finish-the-work

condition: `(?i)(?:\bi(?:'ll| will) leave\b)|(?:\bthe rest (?:is|should be) straightforward\b)`

---
description: Fires when the model defers reachable work.
condition:
  - '(?i)\bi(?:''ll| will) leave\b'
  - '(?i)\bthe rest (?:is|should be) straightforward\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Finish the work you described. Defer only what the user scoped out, and name it in the todo list.

### docs

| rule | source file | scope | interrupt | repeat |
|---|---|---|---|---|
| docs-contract-markers | docs/rules/docs-contract-markers.md | patch calls | never | after-gap 10 |
| docs-update-with-change | docs/rules/docs-update-with-change.md | always applied | - | - |

#### docs-contract-markers

conditions, exactly:

- `(?i)\+[^\n+]{0,200}\bversion [0-9]{1,4}\.[0-9]{1,3}\.[0-9]{1,3}\b`
- `(?i)\+[^\n+]{0,200}\b(?:deprecated|breaking change)\b`

---
description: A patch that touches versions, deprecations, or breaking changes is a contract change.
condition:
  - '(?i)\+[^\n+]{0,200}\bversion [0-9]{1,4}\.[0-9]{1,3}\.[0-9]{1,3}\b'
  - '(?i)\+[^\n+]{0,200}\b(?:deprecated|breaking change)\b'
scope: tool:patch
interruptMode: never
repeatMode: after-gap
repeatGap: 10
---
A patch that touches version numbers, deprecations, or breaking changes is a contract change. Update the changelog and the affected docs in the same patch.

#### docs-update-with-change

Always applied; no condition.

---
description: Behavior and contract changes update docs in the same change.
alwaysApply: true
---
When a change alters behavior, CLI flags, config keys, error strings, or file formats, update the affected docs and the changelog in the same change.

### atlas-v2

| rule | source file | scope | interrupt | repeat |
|---|---|---|---|---|
| atlas-v2-no-workaround | atlas-v2/rules/atlas-v2-no-workaround.md | text | prose-only | after-gap 10 |
| atlas-v2-read-before-edit | atlas-v2/rules/atlas-v2-read-before-edit.md | always applied | - | - |

#### atlas-v2-no-workaround

condition: `(?i)\b(?:workaround|quick fix|hack for now|temporary (?:fix|hack))\b`

---
description: Fires when the model announces a workaround or quick fix.
condition: '(?i)\b(?:workaround|quick fix|hack for now|temporary (?:fix|hack))\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Name the root cause and fix it there. If a workaround is truly needed, write its reason and its removal condition next to the code.

#### atlas-v2-read-before-edit

Always applied; no condition.

---
description: Edit only files you have read; find names with search, not memory.
alwaysApply: true
---
Edit only files you have read in this session. Find definitions and call sites with search before you change a name. Never copy shapes from memory of other projects.

### project-workflow

| rule | source file | scope | interrupt | repeat |
|---|---|---|---|---|
| project-workflow-agents-md-binding | project-workflow/rules/project-workflow-agents-md-binding.md | always applied | - | - |
| project-workflow-slice-first | project-workflow/rules/project-workflow-slice-first.md | text | prose-only | after-gap 10 |
| project-workflow-write-it-down | project-workflow/rules/project-workflow-write-it-down.md | text | prose-only | after-gap 5 |

#### project-workflow-agents-md-binding

Always applied; no condition.

---
description: The workspace instructions are binding for every kind of work.
alwaysApply: true
---
The workspace instructions (AGENTS.md and the files it names) are binding. Re-read them when you start a new kind of work in the repo.

#### project-workflow-slice-first

condition: `(?i)\b(?:rewrite|replace|restructure) (?:the )?(?:whole|entire|all of)\b`

---
description: Fires on wholesale rewrite plans without a decided slice.
condition: '(?i)\b(?:rewrite|replace|restructure) (?:the )?(?:whole|entire|all of)\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Plan the smallest slice that proves the change, and land slices in order. Never rewrite wholesale without a decided plan.

#### project-workflow-write-it-down

condition: `(?i)\bi(?:'ll| will) (?:keep|remember) (?:that|this|it) in mind\b`

---
description: Fires when the model promises to remember instead of writing.
condition: '(?i)\bi(?:''ll| will) (?:keep|remember) (?:that|this|it) in mind\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 5
---
Do not hold that in memory. Add it to the todo list or write it to a file now.

### git-commit

| rule | source file | scope | interrupt | repeat |
|---|---|---|---|---|
| git-commit-no-force-push | git-commit/rules/git-commit-no-force-push.md | exec calls | tool-only | after-gap 1 |
| git-commit-no-placeholder-message | git-commit/rules/git-commit-no-placeholder-message.md | exec calls | tool-only | after-gap 1 |
| git-commit-no-secrets | git-commit/rules/git-commit-no-secrets.md | patch calls | tool-only | after-gap 1 |

#### git-commit-no-force-push

condition: `(?i)(?:\bgit push\b.{0,200}\s--force(?:\s|$))|(?:\bgit push\b.{0,200}\s-f(?:\s|$))`

---
description: Never force-push; recover by fetch and rebase instead.
condition:
  - '(?i)\bgit push\b.{0,200}\s--force(\s|$)'
  - '(?i)\bgit push\b.{0,200}\s-f(\s|$)'
scope: tool:exec
interruptMode: tool-only
repeatMode: after-gap
repeatGap: 1
---
Never force-push. If the push is rejected, fetch and rebase onto the new tip. Use --force-with-lease only when the user asked for it.

#### git-commit-no-placeholder-message

condition: `(?i)\bgit commit\b.{0,200}-m\s+["'](wip|tmp|temp|fix|update|stuff|changes|misc)["'](\s|$)`

---
description: Fires on placeholder commit messages.
condition: '(?i)\bgit commit\b.{0,200}-m\s+["''](wip|tmp|temp|fix|update|stuff|changes|misc)["''](\s|$)'
scope: tool:exec
interruptMode: tool-only
repeatMode: after-gap
repeatGap: 1
---
Write a commit message that names what changed and why. Never commit with a placeholder message.

#### git-commit-no-secrets

condition: `(?i)(?:api[_-]?key|secret|password|token|credential)["']?\s*[:=]\s*["'][A-Za-z0-9+/_=-]{16,}["']`

---
description: Fires on credential-looking literals added by a patch.
condition: '(?i)(?:api[_-]?key|secret|password|token|credential)["'']?\s*[:=]\s*["''][A-Za-z0-9+/_=-]{16,}["'']'
scope: tool:patch
interruptMode: tool-only
repeatMode: after-gap
repeatGap: 1
---
That literal looks like a credential. Never write credentials into the repo. Read them from config or the environment at run time.

### detectors

The `detectors` set is compiled code, not Markdown: three report-only
lanes plus one text rule.
`report: true` marks the v0 report-only state; the body text below is
injected only after promotion, which is a dalgona release decision.
The three streaming lanes fire once per turn at most, then latch. Only the `fabricated-unavailable-tool-call` text rule repeats `after-gap` with gap 1.

| rule | kind | scope | body (used after promotion) |
|---|---|---|---|
| collapse-repetition | lane | text, thinking; all tool sources except the paragraph and near-duplicate sub-lanes | Your output fell into a repetition loop. Stop the loop, and continue with the next concrete step. |
| control-token-leak | lane | text, thinking | Your output leaked control tokens. Stop echoing harness protocol text, and continue the task in plain prose. |
| repetitive-turns | cross-turn lane | text | You have repeated a near-identical message across turns. Change approach, state what differs, and continue. |
| fabricated-unavailable-tool-call | text rule | text | Your previous output imitated an unavailable tool call as inert text instead of taking action. Redo the interrupted step now with your real tools, such as patch for file changes. Do not print or imitate unavailable-tool transcript envelopes. |

`fabricated-unavailable-tool-call` conditions, exactly, with interrupt
mode `always`:

- `(?i)<\s*unavailable-tool-call\b`
- `(?i)\[called\s+tool\s+["'][^"'\r\n]+["']\s+\(no\s+longer\s+available\s+in\s+this\s+session\)`

## Budgets

The loader enforces four byte budgets in load order (user files, then
each selected set, then project files). Instruction text is never
truncated.

- An always-apply body is at most 12288 bytes. A body over the cap
  becomes a rulebook rule with the note `the body is longer than 12288 bytes. dalgona lists this rule in the rulebook instead.`
- The always-apply prompt section holds at most 40960 bytes. When it is
  full, the note is `the always-apply budget of 40960 bytes is full. dalgona lists this rule in the rulebook instead.`
- One rulebook line is at most 4096 bytes. Over it, the note is `the rulebook line is longer than 4096 bytes. dalgona cut the line at 4096 bytes.` and the listing line is cut at 4096 bytes.
- The whole rulebook listing is at most 10240 bytes. Over it, the note
  is `the rulebook listing of 10240 bytes is full. dalgona did not list this rule; read it with rule://<name>.`

A rule past a budget that has no description is skipped with the budget
as the problem reason. Each skipped rule prints the startup notice once:
`rules: dalgona skipped 1 part of your rules. Run "dalgona rules" to see why.`

## Swapping and disabling

- A user rule file with the same name replaces a bundled pack rule or a
  project rule, never the reverse; the load note reads `rule "<name>" from plugin <set> is replaced by <path>`. The fired reminder uses the
  user file's body and settings whole, including `report`.
- `[rules] disabled = ["<name>"]` drops a rule with no problem row.
- `[rule_sets] enabled = []` loads no bundled set; user and project
  rules still load.
- `[rules] watch = false` keeps always-apply and rulebook rules loading;
  stream rules get the watch note `rules.watch is false, so dalgon does not watch this rule` and do not run; the detector lanes get `rules.watch is false, so dalgona does not watch this rule` and hold.
- A user rule file named after a detector lane (for example
  `collapse-repetition.md`) replaces the compiled lane when the session
  builds its rules: the lane stands down and the user rule watches instead.
- A rule with `enabled: false` in its front matter stays loaded but
  inactive.
