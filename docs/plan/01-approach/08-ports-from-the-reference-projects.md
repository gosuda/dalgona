### Ports from the reference projects

This table lists what each step takes from `.references/omp2`, `.references/oh-my-pi` and `.references/grok-build`. A row that says `Idea` takes a behaviour and copies no code, so it needs no notice. A row that says `Code` adds the project's notice in its commit (rule 9). A step that finds it must copy code from a reference adds the notice in the same commit, even when its row says `Idea`. `.references/crush` is read only, for its use of Bubble Tea, and nothing is taken from it.

| Step | Project | What is taken |
|---|---|---|
| Terminal step 9 | grok-build, omp2 | Idea: `NO_COLOR` means no colour; quantize to the terminal profile |
| Terminal step 11 | grok-build | Idea: the timer never truncates, and the label truncates first |
| Terminal step 12 | oh-my-pi, omp2 | Idea: segments shed whole by priority |
| Terminal step 13 | grok-build, omp2 | Idea: a pending confirm replaces the hints, and a double press inside a window |
| Terminal step 15 | oh-my-pi, omp2 | Idea: one key for the full output |
| Terminal step 16 | omp2 | Idea: a 10-line tail, and thinking collapsed |
| Terminal step 17 | oh-my-pi, omp2 | Idea: the open block stays raw |
| Terminal step 18 | oh-my-pi, omp2 | Idea: five visible steps and a pinned block |
| Terminal step 20 | omp2 | Idea: secret-bearing `/login` lines are skipped |
| Terminal step 22 | omp2 | Idea: popup below the prompt; Tab accepts; Esc keeps the draft |
| Terminal step 26 | grok-build | Idea: capped view height with an expand key |
| Terminal step 27 | oh-my-pi | Idea: an Other row |
| Terminal step 28 | grok-build, oh-my-pi | Idea: an atom chip that expands on send |
| Terminal step 29 | omp2 | Idea: directories end in `/` |
| Terminal step 30 | grok-build, oh-my-pi | Idea: `!` on an empty prompt with a distinct mark |
| Terminal step 31 | grok-build, oh-my-pi | Idea: the chip label and a cap of 10 |
| Terminal step 32 | grok-build, omp2 | Idea: a visible queue and pull-back |
| Terminal step 33 | grok-build, omp2 | Idea: double Esc on empty; the phases picker, confirm, execute; `files stay` |
| Terminal step 34 | grok-build | Idea: grouped modal |
| Terminal step 35 | omp2 | Idea: opt-in only while an overlay needs the pointer |
| Terminal step 36 | grok-build, omp2 | Idea: protocol by terminal; notify only when unfocused |
| Terminal step 37 | omp2 | Idea: one startup line with tips |
| Ratchet steps 3 to 6 | oh-my-pi | Code: the gate, the split, the statistic and the plan rules of `packages/coding-agent/src/ratchet/ratchet.ts`, rewritten in Go. Ratchet step 3 adds the MIT notice. |
| Live-debug step 3 | oh-my-pi | Tool: the `tui` tool files are copied into `.omp/tools`, which git ignores. Nothing of it ships. |

