#### Web step 38. Accessibility gates

- Keyboard path test. `web/src/lib/components/App.dom.test.ts`, describe `keyboard paths`, using `userEvent.tab()`. The order is: skip link, Start thread, thread list, transcript, Jump to latest (when shown), request card, composer, and Send. Each path completes by keyboard alone:
  - Send a message.
  - Accept an approval.
  - Open and close each panel.
  - Open review with Ctrl+O.
- Browser checks.
  - `web/checks/target-size.js`: every visible `button`, `a`, `input`, `select`, `summary` and `[role=option]` is at least 24x24 CSS px (WCAG 2.5.8). Buttons are at least `--control-height` tall; under `pointer=coarse` emulation, at least `--control-height-coarse`.
  - `web/checks/reduced-motion.js`, with media `prefers-reduced-motion=reduce` during a Working turn: `document.getAnimations().length === 0`, and the elapsed text is unchanged after 3 s with no new item.
  - `web/checks/forced-colors.js`, with `forced-colors=active`: tags and the point button have a non-zero border width.
- Contrast. The unit gate from step 8 covers contrast for all four themes.
- Spec (`docs/spec/web.md`). New `### Accessibility` lists the gates:
  - contrast per theme (4.5:1 text, 3:1 boundaries)
  - the keyboard paths
  - reduced motion
  - target size 24 px minimum and control heights
  - form text 16 px
  - forced colours
  - live regions (one status region, one alert region, transcript not live)
- Test. The keyboard-path test, plus the three checks. Fails before for paths whose controls do not exist before steps 23 to 32. Those cases join this test as each step lands, so this step only adds the cross-path test and the cumulative spec.
- Lands after: Steps 8, 14 and 23 to 37.

Anchor: Richard Feynman, Edsger W. Dijkstra.

##### Findings mapped to steps

From the web review list:

| Finding | Steps |
|---|---|
| Approval-queue note outside `.shell` | 7 |
| Sidebar cap on the wrapper | 12 |
| Missing `min-width:0` at 320 px | 37 (and 5, 17, 20) |
| Approval actions not sticky; diff capped at 16rem | 21, 17 |
| No jump-to-latest | 19 |
| Errors name a Reconnect control that is not on screen | 6 |
| Selected thread colour-only; forced colours | 11 |
| Live regions mount with text | 5 |
| `role=log` on streamed deltas | 14 |
| Focus loss on removed controls | 1, 13, 5, 21 |
| Fonts named but not loaded | 9 |
| Web drops the `seq` guard | 2 |
| `validation.ts` not checked against the Codex schema | 3 |
| Live file-change approvals show no diff | 22 |

Other medium findings from the six reviews:

| Finding | Steps |
|---|---|
| Cancel hides the interrupt | 21 |
| Output cap has no cue | 18 |
| Composer not docked at narrow widths | 24 |
| 14 px select on mobile | 24 |
| Paths overflow | 17, 37 |
| Heading scale | 9 |
| Quiet button boundary contrast | 8 |
| Danger dot shown for idle | 20 (connection shown as text only) |
| Hover overrides the selection | 11 |
| Forced colours for buttons and banners | 8, 38 |
| Keyword sweep on focus | 16 |
| Working shown in success green | 10 |
| Input radius mismatch | 8 (`radiusControl` for all inputs) |
| Plural helper | 7 |
| Raw sandbox value | 20 |
| Mixed vocabulary | 10, 16, Decisions |
| Verb-first labels | Decisions, 13, 21 |
| Dead-end "No models" | 29 |

##### Terminal features mapped to browser components

| Terminal feature | Browser component |
|---|---|
| Tool cards | `CommandCard`, `ToolCard` |
| Diffs | `DiffCard` |
| Streaming Markdown | `Markdown` |
| Reasoning collapse | `ThinkingRow` and verbosity |
| Plan panel | `PlanPanel` |
| Subagent block | `SubagentBlock` |
| Status band | `StatusBand` |
| Working indicator | `StateTag` live dot and counter |
| Four themes and `NO_COLOR` | four themes, `auto`, forced colours |
| Notifications | step 36 |
| Welcome and exit lines | empty-state welcome (16); `Disconnected from dalgona.` notice (6) |
| Terminal title | document title (20) |
| Multi-line editing | textarea, Shift+Enter |
| History | 27 |
| Slash palette | `CommandPalette` |
| `@file` completion | 33 |
| Paste chips and image paste | 35 |
| `!` shell mode | 34 |
| Approval card | `ApprovalCard` |
| Pickers | `ModelPanel`, `ResumePanel`, `TreePanel` |
| Visible follow-up queue | `FollowupQueue` |
| Esc interrupt guard | 26 |
| Help overlay | `KeyListPanel` |
| Mouse | native |
| Rewind gesture | `Rewind to here` and `RewindPanel` |
| Ask tool | question card (ask workstream, Step 8), placed in the dock by that step |
| `/settings`, `/login`, `/model`, presets and the other slash commands | panels 28 to 32 |
| Review on Ctrl+O | `ReviewView` |
| Verbosity | 18 |

Not mapped: the external editor key, which is out of scope.

