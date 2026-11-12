# Design

### Purpose

This file sets one visual system for both interfaces of dalgona: the terminal interface in `internal/tui` and the web interface in `web/`. Both interfaces use the same themes, colours, type, components, motion, words, icons and marks. Read this file before you add or change a colour, a type size, a component, a glyph, a motion, a status item or a mark.

To change the system, get the owner's approval first. One commit changes both interfaces, so they do not drift apart. When the owner rejects an option, record the option and the reason in `docs/decisions/`.

### Principles

- A little human warmth: the rabbit mark and plain words make the tool feel friendly. Keep that warmth small.
- Restraint: use few colours, few sizes and few weights. Remove any element that carries no meaning.
- Words carry state: every state has a word. Colour and icons add to the word. They never replace it.
- Nothing moves unless it changes: only the live dot and the elapsed time move.
- One point per view: each view has one solid fill, the point (see The point and the thread hue).

The terminal follows four stability rules:

- A finished row is written once and never redrawn.
- The dock keeps its height unless the user changes it.
- A value that changes with time keeps its place and its width.
- The client never clears the screen.

### Colour

The setting `theme` takes six values.

|Value|Behaviour|
|---|---|
|`auto` (default)|Follows the system. Uses `night` or `day`.|
|`auto-soft`|Follows the system. Uses `soft-night` or `soft-day`.|
|`night`|Fixed.|
|`soft-night`|Fixed.|
|`day`|Fixed.|
|`soft-day`|Fixed.|

The two night themes take their grounds from Radix Colors 3.0.0 slate dark. Every other value comes from Flexoki.

|Role|night|soft-night|day|soft-day|Rule|
|---|---|---|---|---|---|
|`canvas`|#111113|#212225|#FFFCF0|#E6E4D9|Page ground in the browser. The terminal paints no background.|
|`surface`|#18191b|#272a2d|#E6E4D9|#FFFCF0|Raised panel, card, field, status log and diff row ground.|
|`tone`|#212225|#2e3135|#CECDC3|#CECDC3|Fill of a secondary control, a disabled control, a selected row, a command block and an edit tag.|
|`line`|#363a3f|#43484e|#B7B5AC|#B7B5AC|Hairline rules: card and log borders, row dividers, option borders.|
|`strong`|#696e77|#777b84|#6F6E69|#6F6E69|Strong borders: buttons, fields, option marks and underlines.|
|`text`|#CECDC3|#CECDC3|#100F0F|#100F0F|Body text and the focus ring. In the terminal, the default foreground.|
|`dim`|#9F9D96|#9F9D96|#403E3C|#403E3C|Secondary text: notes, captions, descriptions, key hints. The words Done and Waiting, and the item status word Declined.|
|`faint`|#6F6E69|#6F6E69|#6F6E69|#6F6E69|Decoration only: tree rails. Never body text.|
|`accent-text`|#DA702C|#EC8B49|#71320D|#71320D|Links, the keyword mark, the terminal product name and selection markers.|
|`failed-text`|#E8705F|#F89A8A|#6C201C|#6C201C|The word Failed and error text. Bold when it marks a failed state or an error. Never a fill.|
|`added-text`|#879A39|#A0AF54|#3D4C07|#3D4C07|Added lines and success text.|
|`warning-text`|#D0A215|#D0A215|#664D01|#664D01|Warnings.|

Only `canvas`, `surface`, `tone` and the point are fills. The text roles `failed-text`, `added-text` and `warning-text` are never fills. No colour outside this table may appear.

When the terminal shows fewer colours, or when `NO_COLOR` is set, every colour role drops to the terminal's default foreground. Bold and dim stay. Every state keeps its word.

### The point and the thread hue

Each view has at most one solid accent fill, the point. The order is: the primary action of a pending decision; else the live tag; else Send in the browser. The terminal has no Send button, so an idle terminal view has no fill, and the prompt mark carries the thread hue as its text colour. The point takes its colours from the thread hue. The product mark is not the point.

A failed state is the word Failed in bold `failed-text`. It is never a fill.

Each thread has one hue: `fnv1a32(threadID) % 4`. `fnv1a32` is FNV-1a 32-bit over the UTF-8 bytes of the thread id. In Go, use `hash/fnv` `New32a`. In TypeScript, use offset basis 2166136261, prime 16777619, `Math.imul`, and `>>> 0`.

|Index|Hue|Night and soft-night: fill|Night and soft-night: ink|Day and soft-day: fill|Day and soft-day: ink|
|---|---|---|---|---|---|
|0|Orange|#EC8B49|#100F0F|#9D4310|#FFFCF0|
|1|Cyan|#3AA99F|#100F0F|#1C6C66|#FFFCF0|
|2|Blue|#66A0C8|#100F0F|#1A4F8C|#FFFCF0|
|3|Magenta|#E47DA8|#100F0F|#87285E|#FFFCF0|

In the browser, the hue fills the point and an 8 px dot beside each thread in the list. In the terminal, the hue fills the live tag and colours the prompt mark with the fill colour. The dot and the prompt mark carry no information that the text does not also carry.

### Type for the web

|Use|Face|Stack|
|---|---|---|
|Titles and control labels|Bricolage Grotesque|`"Bricolage Grotesque", sans-serif`|
|Text, and Hangul through the fallback|Hanken Grotesk, then Pretendard|`"Hanken Grotesk", "Pretendard Subset", sans-serif`|
|Code and terminal text|System monospace|`ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`|
|Nerd Font glyphs|Symbols Nerd Font Mono 3.5.1, subset|CSS family `"Symbols Nerd Subset"`. Used only for the glyphs in Icons.|

Elapsed time and percentages use `font-variant-numeric: tabular-nums`. Line heights are unitless.

### Scale

Each non-colour token has one value. The browser reads these tokens from `web/src/lib/theme/scale.ts`.

|Token|CSS custom property|Value|Where it is used|
|---|---|---|---|
|`sizeCaption`|`--size-caption`|12px|Labels, notes, key hints, edit tags, diff text, and the sidebar overline|
|`sizeSmall`|`--size-small`|13px|Status tags, verb and target, time, working directory, option descriptions, decision buttons, counts, Open file|
|`sizeBody`|`--size-body`|15px|Body text, reason line, field input, primary decision button|
|`sizeHeading`|`--size-heading`|22px|Question title|
|`sizeTitle`|`--size-title`|26px|Card titles; empty-state heading|
|`sizeDisplay`|`--size-display`|30px|Command text in the approval card|
|`leadingTight`|`--leading-tight`|1|Buttons and status tags|
|`leadingBody`|`--leading-body`|1.5|Body text and field input|
|`leadingCode`|`--leading-code`|1.4|Verb, target and note text|
|`weightText`|`--weight-text`|400|Body text, field input, verb and target|
|`weightStrong`|`--weight-strong`|700|Failed word and error text; selected thread title; destructive button|
|`weightTitle`|`--weight-title`|600|Headings, decision buttons, labels, edit tag, Open file|
|`space1`|`--space-1`|4px|Edit tag padding|
|`space2`|`--space-2`|8px|Decision button grid gap; button padding; skip link padding|
|`space3`|`--space-3`|12px|Control gaps; skip link offset; thread row padding|
|`space4`|`--space-4`|16px|Row padding; button padding; skip link padding|
|`space6`|`--space-6`|24px|Approval card padding.|
|`space8`|`--space-8`|32px|Review body height limit.|
|`radiusTag`|`--radius-tag`|8px|Status tags|
|`radiusControl`|`--radius-control`|10px|Buttons|
|`radiusCard`|`--radius-card`|12px|Cards and the status log|
|`borderWidth`|`--border-width`|1px|Every hairline border in the component rows|
|`controlHeight`|`--control-height`|36px|Minimum height of buttons and selects|
|`controlHeightCoarse`|`--control-height-coarse`|44px|Buttons and selects under `@media (pointer: coarse)`|
|`targetMin`|`--target-min`|24px|Minimum pointer target. The target-size check requires every visible button, link, input, select and summary to be at least 24 by 24 CSS px, the WCAG 2.2 success criterion 2.5.8 minimum.|
|`motionBreath`|`--motion-breath`|1.8s|Period of the breathing dot; reduced-motion reset|
|`motionQuick`|`--motion-quick`|120ms|Button colour and border transitions; reduced-motion reset|
|`ease`|`--ease`|ease-out|Button transition timing|

A component row keeps its own literal value when no token has that value. So the 14 px labels of buttons and options, the weight 650 of buttons and tags, the 14 px radius of the composer field, and the 7 px and 8 px radii stay as their rows give them. Bold text, such as the word Failed, uses `weightStrong`. The target-size check reads `targetMin`.

### Components

Each table gives the part, its geometry, its type and its colour role. Browser sizes are CSS pixels. Terminal sizes are cells and rows. Each value appears once in its table.

#### Browser controls

|Part|Geometry|Type|Colour|
|---|---|---|---|
|Button|Minimum height 36 px. Padding 0 20 px. Radius 10 px. Border 1 px. Inline flex, centred, no wrap.|Bricolage Grotesque, weight 650, 14 px, line height 1|Set by variant|
|Point button|Border 1 px.|As button|Fill and border: point. Label: point ink|
|Secondary button|Border 1 px.|As button|Fill `tone`. Border `strong`. Label `text`|
|Quiet button|No fill. No border. Padding 0 12 px.|As button|Label `dim`|
|Destructive button|No fill. Border 1 px.|Bricolage Grotesque, weight 650, 14 px, line height 1|Border and label `failed-text`. The label keeps the weight of every button label.|
|Disabled button|Cursor not-allowed.|As button|Fill `tone`. Border `line`. Label `dim`|
|Focus ring|Outline 2 px solid, offset 2 px. Drawn on every focusable control.|n/a|`text`|
|Field|Height 60 px. Padding 8 px 8 px 8 px 20 px. Gap 12 px. Radius 14 px. Border 1 px.|n/a|Fill `surface`. Border `strong`|
|Field input|Width 100%. Height 100%. Padding 0. Border 0. Background transparent. Focus ring drawn.|Hanken Grotesk, weight 400, 15 px, line height 1.5|`text`|
|Field point button|Height 42 px. Radius 8 px. Flex none.|As button|Point|
|Composer|Grid: `minmax(0,1fr)` and `auto`. Gap 12 px. Centred vertically. Top margin auto.|n/a|n/a|
|Button row|Gap 12 px. Top margin 28 px.|n/a|n/a|
|Status log|Four rows, 44 px each. Columns 92 px, 44 px, `minmax(0,1fr)` and 64 px. Gap 12 px. Row padding 0 16 px. Radius 12 px. Border 1 px. Overflow hidden. Rows divided by 1 px.|n/a|Fill `surface`. Border and dividers `line`|
|Status tag|Height 28 px. Minimum width 84 px. Padding 0 8 px. Gap 8 px. Radius 8 px. Start aligned, no wrap.|Bricolage Grotesque, weight 650, 13 px, line height 1|`dim` (Done, Waiting, and the item status Declined)|
|Live tag (Working)|As status tag, with a point fill.|As status tag|Fill point. Label point ink|
|Dot|6 px circle. Background `currentColor`.|n/a|Takes the label colour|
|Failed tag|No fill. No border. The word only.|Bricolage Grotesque, weight 700, 13 px, line height 1|`failed-text`, bold|
|Verb|No wrap.|Monospace, weight 400, 13 px, line height 1.4|`text`|
|Target|Grid: `minmax(0,1fr)` and 112 px. Gap 12 px. Minimum width 0.|Monospace, weight 400, 13 px, line height 1.4|`text`|
|Note|Right aligned. No wrap.|12 px, line height 1.4|`dim`. Exit note `failed-text`, bold|
|Time|Right aligned. No wrap. Tabular numbers.|13 px|`text`|

#### Browser cards

|Part|Geometry|Type|Colour|
|---|---|---|---|
|Board|Two columns of 460 px. Column gap 24 px. Row gap 20 px. Rows 420 px and 248 px. The approval card spans both rows.|n/a|n/a|
|Card|Padding 20 px. Radius 12 px. Border 1 px. Flex column.|n/a|Fill `surface`. Border `line`|
|Approval card|Padding 24 px.|n/a|As card|
|Card title|Margin 0.|Bricolage Grotesque, weight 600, 26 px, line height 32 px|`text`|
|Command block|Top margin 22 px. Minimum height 112 px. Padding 18 px 20 px. Left border 4 px. Flex column, centred vertically.|n/a|Fill `tone`. Left border `text`|
|Command|No wrap.|Monospace, weight 600, 30 px, line height 38 px|`text`|
|Working directory|Top margin 5 px.|Monospace, 13 px, line height 20 px|`dim`|
|Reason|Top margin 18 px.|Hanken Grotesk, 15 px, line height 23 px|`text`|
|Decision buttons|Grid of two columns. Gap 8 px. Top margin auto.|n/a|n/a|
|Decision button|Minimum height 42 px. Border 1 px. Radius 7 px. Padding 0 13 px. Space between.|Bricolage Grotesque, weight 600, 13 px|No fill. Border `strong`. Label `text`|
|Primary decision button|Full width. Minimum height 48 px.|Bricolage Grotesque, weight 600, 15 px|Fill and border point. Label point ink|
|Quiet decision button|Full width. Minimum height 36 px. No border. Centred. Gap 10 px. Underlined, underline offset 4 px.|Bricolage Grotesque, weight 600, 13 px|Label `dim`|
|Shortcut key|n/a|Monospace, 12 px|Inherits the label|
|Label|n/a|Hanken Grotesk, weight 600, 12 px, line height 16 px|`dim`|
|Question title|Top margin 6 px. Bottom margin 12 px.|Bricolage Grotesque, weight 600, 22 px, line height 27 px|`text`|
|Option|Height 54 px. Border 1 px. Radius 8 px. Padding 7 px 11 px. Columns 20 px and `minmax(0,1fr)`. Rows 20 px and 17 px. Column gap 9 px.|n/a|Border `line`|
|Option mark|16 px circle. Border 1 px. Top margin 5 px. Spans two rows. Centred.|n/a|Border `strong`|
|Selected option|Fill `tone`. Leading bar 2 px wide. Radio dot 8 px inside the mark.|n/a|Fill `tone`. Bar and dot `text`|
|Option choice|n/a|Hanken Grotesk, weight 600, 14 px, line height 19 px|`text`|
|Option description|n/a|Hanken Grotesk, 13 px, line height 17 px|`dim`|
|Other row|Height 42 px. Top margin 8 px. Border 1 px. Radius 8 px. Padding 0 11 px. Gap 14 px.|Hanken Grotesk, weight 600, 14 px|Border `line`. Label `text`|
|Other input|Flex 1. Minimum width 0. Border 0 except a 1 px bottom border. Padding 5 px 0. Focus ring drawn.|Hanken Grotesk, 13 px|`text`. Underline `strong`|
|Question buttons|Top margin auto. Gap 9 px. End aligned. Minimum width 92 px. Minimum height 38 px.|Primary button: 14 px|As decision buttons|
|Meta row|Height 26 px. Gap 9 px. Centred.|n/a|n/a|
|Edit tag|Padding 4 px 7 px. Radius 4 px.|Bricolage Grotesque, weight 600, 12 px|Fill `tone`|
|File path|Flex 1.|Monospace, 12 px|`text`|
|Count|n/a|Monospace, weight 600, 13 px|Added count `added-text`, bold. Removed count `failed-text`, normal weight|
|Diff block|Top margin 12 px.|n/a|n/a|
|Diff line|Height 24 px. Columns 29 px, 17 px and `minmax(0,1fr)`. Padding 0 5 px. Radius 3 px. Centred.|n/a|Ground `surface`|
|Line number|n/a|Monospace, 12 px|`dim`|
|Sign|n/a|Monospace, 12 px, normal style|Takes the line colour|
|Code|Whitespace kept.|Monospace, 12 px|Takes the line colour|
|Added line|n/a|n/a|`added-text` on `surface`|
|Removed line|n/a|Normal weight|`failed-text` on `surface`|
|Open file button|Top margin auto. Aligned to the start. No fill. Bottom border 1 px. Padding 3 px 0.|Bricolage Grotesque, weight 600, 13 px|Label `text`. Underline `strong`|

#### Terminal rows

|Part|Geometry|Type|Colour|
|---|---|---|---|
|Line|Height 1 row. No wrap.|System monospace, regular|`text`|
|Prompt line|Columns: the prompt mark, then the text in the free column.|Bold|`text`|
|Prompt mark|The character `›` (U+203A, East Asian Width `N`).|Bold|Point colour of the thread hue|
|Answer line|Columns: a gutter, then the text in the free column.|System monospace|`text`|
|Spacer|Blank rows between blocks.|n/a|n/a|
|Tool row|Height 1 row. Columns: two blank cells and the status word in 8 cells (the same 10 cells as the live tag), verb, target in the free column, meta, time.|Verb bold|Status word `dim`. Verb `text`. Target `text`. Meta `dim`, tabular numbers. Time `dim`, right aligned, tabular numbers|
|Failed word|In the 8-cell status field.|Bold|`failed-text`. No fill|
|Live tag|Width 10 cells: the dot, one space, and the state word in 8 cells. Height 1 row.|Bold|Point fill while a turn runs and no decision is open. Dot and label point ink. No fill in every other state|
|Live dot|The first cell of the live tag. Alternates between U+25AA and U+25AB every 500 ms. Under `DAL_NO_MOTION` it stays on U+25AA.|n/a|Point ink|
|Diff line|Columns: the line number, then the code in the free column.|Monospace|Line number `dim`. Added `added-text`. Removed `failed-text`, normal weight|
|Hint line|Shown in the live row when no turn runs.|n/a|`dim`|
|Dock|The last rows of the screen, from the top: the composer, the open panel when there is one, the live row, and the status band. The dock changes height only on a key press, a paste, or the open or close of a panel.|n/a|No background|
|Composer|One row per line of input. The composer grows only as the user adds lines, up to a fixed limit, and scrolls inside itself beyond it. Columns: the prompt mark, then the text in the free column. The terminal's cursor rests on the first row, and the client draws no cursor glyph.|n/a|Prompt mark point colour. Text `text`|
|Live row|One row, below the open panel and above the status band. Fields from the left: the live tag in 10 cells (the dot, one space, the state word in 8 cells), the elapsed time in 7 cells, the progress fields, then free text. Nothing is right-anchored.|As tool row|As tool row|
|Status band|One row of fixed fields. The left group holds `model` and the icons. The right group holds `context` and `mark`.|n/a|`dim`|

#### Terminal panels

|Part|Geometry|Type|Colour|
|---|---|---|---|
|Open panel|At most one, below the composer and above the live row. Full width of the dock. Its height is fixed for its kind, in rows, and never changes while it is open. When the window is short, it has fewer rows and scrolls inside itself.|n/a|n/a|
|Caption|One row, above the body.|System monospace, regular|`dim`|
|Body|Width 56 cells. Height is the open panel's fixed height. Content longer than that scrolls inside the panel.|System monospace, regular|`text`|
|Rail|`│` at the start of each row. `┌` on the first row. `└─ ` before the key row.|Monospace|`faint`|
|Selected row|Leading `┃` in `text`. No fill, because the terminal paints no background. Bold.|Bold|`text`|
|Selection marker|`●` before the selected label.|Bold|`accent-text`|
|Recommended label|n/a|Bold|`accent-text`|
|Title line|Question or heading.|Bold|`text`|
|Detail line|n/a|Regular|`dim`|
|Key row|Key hints.|Regular|`dim`|

### Motion and words

The live tag and the Working rows each carry one breathing dot. The elapsed time on a Working row ticks once a second. The dot and the tick use these values.

|Property|Browser|Terminal|
|---|---|---|
|Dot size|6 px|One cell|
|Animation|`breathe`, 1.8 s, ease-in-out, infinite|U+25AA and U+25AB, switched every 500 ms|
|Opacity at 50 %|0.4|n/a|
|Colour|`currentColor`|`currentColor`|
|Stopped by|`prefers-reduced-motion: reduce`|`DAL_NO_MOTION`, which stops the dot only|

In the browser, reduced motion stops the dot and the elapsed time. In the terminal, `DAL_NO_MOTION` stops only the dot, and the elapsed time still updates once a second. The elapsed time keeps its tabular figures so that digits do not change width.

A duration has one form. Under one second in a finished row: `<n> ms`. Under one minute: `<s>s`. Under one hour: `<m>m <ss>s`. From one hour: `<h>h <mm>m`. A ticking duration sits in a field of 7 cells, right-aligned. Token counts are not shown. The one exception is the refusal text of a rate-limit wait, which reads `rate limited for <s> s, over the <limit> s wait limit`.

The state words are written in sentence case. Each has at most 7 cells, so each fits the terminal's state field of 8 cells. Both clients use the same words.

|Word|Colour and form|
|---|---|
|Working|Live tag: point fill, point ink label, breathing dot.|
|Done|`dim`.|
|Failed|Bold `failed-text`. No fill.|
|Waiting|`dim`. Shown with a note such as needs approval.|
|Declined|`dim`. An item status, not a state.|
|Ready|`dim`. The thread waits for a prompt.|
|Stopped|`dim`. Shown for an interrupted turn.|
|Offline|`warning-text`. Shown when the connection to the daemon is lost.|

### Status band

The status band shows these items, in this order.

|Order|Id|Shows|Hideable|
|---|---|---|---|
|1|`model`|The model name as text|No|
|2|`effort`|One icon|Yes|
|3|`approval`|One icon|Yes|
|4|`sandbox`|One icon|Yes|
|5|`folder`|Browser: one icon, with the folder name as its `title` and `aria-label`. Terminal: the glyph and the folder name, cut to 16 cells.|Yes|
|6|`queue`|One icon and the count. Shown only when a follow-up is queued.|Yes|
|7|`context`|A moon gauge and the percent of the context window in use|Yes|
|8|`mark`|Browser: the small product mark. Terminal: the product name.|Yes|

The setting `status_hide` lists item ids to hide. Its default is empty. Its wire name is `statusHide`. Setting `model` fails with `status_hide: model cannot be hidden`.

When the row does not fit, whole items drop in this order: `folder`, `queue`, `effort`, `mark`. The items `model`, `context`, `approval` and `sandbox` never drop. If they still do not fit, `model` is cut from the right with `…` in the browser and with `...` in the terminal.

Terminal layout: one row of fixed fields. `model` and the icons sit in the left group. `context` and `mark` sit in the right group. Items are separated by spacing, not by a glyph. The status text, the moon gauge and the percent use `dim`. Free text in the band (the model name and the folder name) is cut to a fixed number of cells, so the right group keeps its place.

Browser layout: the thread header has the title at the left. A right-aligned margin column holds three rows: `model`; then `effort approval sandbox folder queue`; then `context mark`. Below 640 px, the margin column becomes one row under the title. The same drop order applies. Every icon has an `aria-label` that states its value in words.

### Icons

The setting `icons` takes `auto` (default), `unicode` or `nerd`. The browser always draws Nerd glyphs from its own subset font, `symbols-nerd-subset.woff2`. The subset comes from Symbols Nerd Font Mono 3.5.1. Its licences are MIT for the font, Apache-2.0 for Material Design Icons and SIL OFL 1.1 for Weather Icons. The browser ignores the `icons` setting.

In the terminal, `auto` starts with `unicode`. When `KITTY_WINDOW_ID` is set, the client sends the capability request `kitty-query-font_family` once at start. When the reply arrives, the client reads the value after `=`. When the value matches `(?i)nerd ?font` or `NF[MP]?(-|$)`, the client switches to `nerd`. If the reply has no `name=` prefix, the whole reply is the value. With no reply, `unicode` stays. The client sends no other probe and sets no timeout.

|Item|Value|Nerd glyph|Code point|Unicode glyph|Code point|
|---|---|---|---|---|---|
|Effort|low|`md-gauge_empty`|U+F0873|⣀|U+28C0|
|Effort|medium|`md-gauge_low`|U+F0875|⣤|U+28E4|
|Effort|high|`md-gauge`|U+F029A|⣶|U+28F6|
|Effort|max|`md-gauge_full`|U+F0874|⣿|U+28FF|
|Approval|ask|`md-hand_back_right`|U+F0E47|?|U+003F|
|Approval|auto|`md-robot_outline`|U+F167A|✓|U+2713|
|Approval|yolo|`md-fire`|U+F0238|!|U+0021|
|Sandbox|workspace-write|`md-package_variant_closed`|U+F03D7|⊡|U+22A1|
|Sandbox|none|`md-alert_outline`|U+F002A|⊘|U+2298|
|Folder|any|`md-folder_outline`|U+F0256|▸|U+25B8|
|Queue|count above 0|`md-timer_sand`, then the count|U+F051F|`+`, then the count|U+002B|
|Context|fraction f|Moon gauge: 15 glyphs U+E38D to U+E39B, index round(f × 14). The percent follows.|U+E38D to U+E39B|The percent alone, with no glyph|n/a|

Map the ordered effort levels onto the four effort glyphs by rank: `round(rank × 3 / (levels − 1))`. The effort item shows only when the model has two or more levels.

Every Unicode glyph that the terminal puts in a fixed field or in the status band has East Asian Width `N`, `Na` or `H`, so a fixed field has the same cells in every terminal setting. A test checks each Unicode glyph token with `golang.org/x/text/width`. Nerd Font glyphs are private-use characters and are exempt: each counts one cell, and one space follows it. The prompt mark `›` (U+203A) has East Asian Width `N`. Text that is cut in the terminal ends with `...`. The terminal measures all text with the method of its renderer, shows every character as typed, and replaces none.

The terminal key list (`?`) shows one legend line for each shown item, so that every icon has a text meaning.

### Product marks

The mark is a crescent moon with a small smiling rabbit. The rabbit rests its chin on the inner curve of the moon. The rabbit is named 고나 (Gona). 달 (moon) + 고나 = 달고나 (dalgona), the caramel candy. `dalgona` is a caramel candy with the crescent and the rabbit pressed in as a line. Its highlight is one flat shape of the mark, not a gradient and not an effect that a component may copy. `dal` is the crescent and the rabbit alone, in one colour.

|File|Use|
|---|---|
|`assets/brand/dalgona.svg`|`dalgona` at 24 px and above. App icon, start screen and README.|
|`assets/brand/dalgona-small.svg`|`dalgona` below 24 px. Favicon and status band.|
|`assets/brand/dal.svg`|`dal` at 24 px and above. README.|
|`assets/brand/dal-small.svg`|`dal` below 24 px. Favicon and status band.|

The candy uses fixed brand colours in every theme. Face #DA702C. Rim #9D4310. Highlight #FFFCF0 at 45 % opacity. The `dal` mark uses `currentColor`, which takes the `text` role. `dalgona` always shows the candy mark. `dal` always shows its one-colour mark. The `dal` favicon carries its own `prefers-color-scheme` rule: #100F0F on light, #CECDC3 on dark.

The SVG files hold the geometry. The construction values are:

|Element|Value|
|---|---|
|Canvas|viewBox 0 0 120 120|
|Candy face|Circle, centre (60, 59), radius 50|
|Candy rim|Circle, centre (60, 66), radius 50|
|Crescent|Centre (60, 59). Radius R = 33. Angle −40°. Line width 4.0. Inner offset 0.576. Inner radius 0.803.|
|Rabbit head|Radius 0.21 × R. Eyes are dots.|
|Ears, full|x 0.58, y −1.48, rx 0.24, ry 0.96, rotation 7°. Position and radii in head-radius units.|
|Ears, small|x 0.62, y −1.44, rx 0.30, ry 0.96, rotation 10°. Position and radii in head-radius units.|

Below 24 px, use the small files. They carry the small cut.

The name 고나 appears only in this file, in the README and in image alternative text. The interface shows no text about the rabbit.

|Mark|Alternative text|
|---|---|
|`dalgona`|a caramel candy stamped with a crescent moon and the rabbit Gona|
|`dal`|a crescent moon with the rabbit Gona|

### Accessibility

|Topic|Rule|
|---|---|
|Text contrast|Text meets 4.5:1 against its ground, using the role table in Colour. The terminal paints no background, so there the ratio holds for the grounds of the table: the client picks the night or the day palette from the background colour that the terminal reports.|
|Interface contrast|Interface parts, such as control edges and the focus ring, meet 3:1 against their ground.|
|Diff rows|In `night`, added and removed text on `surface` meets 4.5:1. Diff rows use `added-text` and `failed-text` on `surface`.|
|State|No state is shown by colour alone. Every state keeps its word, and every icon has a text meaning.|
|Focus|Every focusable control draws a 2 px solid `text` ring with a 2 px offset. No rule sets `outline: 0` without drawing this ring.|
|Reflow|The browser reflows at 320 px width without two-dimensional scrolling.|
|Live regions|A live region exists in the page before its text changes.|
|Reduced motion|In the browser, with `prefers-reduced-motion: reduce`, the breathing dot and the elapsed-time tick stop. In the terminal, `DAL_NO_MOTION` stops only the dot.|
|Minimum size|Controls have a minimum height of 36 px.|
|Terminal colour|Under `NO_COLOR` or with fewer colours, every state keeps its word (see Colour).|

### How to change this file

1. Get the owner's approval before you change a value, a component, a glyph, a motion, a status item or a mark.
2. Change `internal/tui` and `web/` in the same commit, so that the two interfaces stay the same.
3. Record each rejected option, with its reason, in `docs/decisions/`.
4. Check the accessibility gates in the section Accessibility before you commit.
