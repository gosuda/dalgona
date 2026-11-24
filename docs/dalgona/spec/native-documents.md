# Native documents

## Status and scope

This document specifies required implementation work for the Go and Gio client.
It does not claim that these components exist or have passed their release gates.

The client MUST implement every component in this specification before its first complete desktop release.
An error presentation MUST NOT count as support for valid input within the declared grammar and limits.

Read this with [the client specification](native.md), [components](native-components.md), [platform integration](native-platform.md), and [verification](native-verification.md).
The [native client decision](../decisions/native-client.md) owns the process and language choice.

## D01. Choose the document and figure boundaries

The document package MUST keep text, structure, selection, and navigation in Go.
Gio MUST draw the interface and receive input.
The client MUST NOT create HTML, a WebView, a browser process, or a CSS layout engine for document presentation.

The implementation MUST use these boundaries:

| Owner | Required responsibility |
|---|---|
| `internal/dalgona/gui/document` | Parse Markdown, maintain document revisions, shape text, map positions, manage selection, and emit Gio operations. |
| `internal/dalgona/gui/figure` | Compile diagrams and math, decode static SVG, supervise renderer children, and validate figure results. |
| `internal/dalgona/gui/terminal` | Maintain terminal cells, terminal selection, scrollback, and terminal input encoding. |
| `internal/dalgona/gui/platform` | Supply fonts, clipboard formats, native dialogs, accessibility, and window handles. |

The implementation MUST choose a supervised renderer child for expensive figure work.
It MUST keep ordinary document layout in the GUI process.

| Candidate | Consequence | Decision |
|---|---|---|
| Render everything in the GUI process | A noncooperative diagram layout can retain resources after a context timeout. | Rejected. |
| Keep text local and render figures in a child | A deadline can terminate the actual computation while the interface remains usable. | Selected. |

The current [terminal diagram implementation](../../../internal/dal/tui/diagram.go) already records this problem through its D2 busy gate.
The GUI MUST NOT copy its goroutine timeout as a hard cancellation mechanism.

## D02. Fix the dependency decisions

The implementation MUST reuse existing Go dependencies where their contracts fit.
An indirect dependency promoted to direct use MUST receive its own row in the [dependency decision](../../dal/decisions/dependencies.md).

| Function | Selected implementation | Required boundary |
|---|---|---|
| Markdown parsing | Existing `github.com/yuin/goldmark` v1 line with GFM extensions | Consume its AST directly. Do not invoke its HTML renderer. |
| Code highlighting | Existing `github.com/alecthomas/chroma/v2` | Consume token ranges directly. Do not create ANSI or HTML. |
| Text shaping | Gio text shaping and its compatible Go text dependencies | Keep one pinned font and shaping revision across document and figure work. |
| D2 | Existing `oss.terrastruct.com/d2` with its bundled Dagre layout | Deny filesystem imports, remote resources, and external layout plugins. |
| Mermaid | The web client's pinned `beautiful-mermaid` bundle | Execute its synchronous SVG algorithm inside the existing pure Go QuickJS runtime. |
| Nomnoml | The web client's pinned nomnoml and graphre bundles | Execute `renderSvg` inside QuickJS without a canvas or browser. |
| Math | An owned bounded parser and box layout using OpenType MATH metrics | Implement the grammar in D09 and preserve its semantic tree. |
| Static SVG | An owned bounded decoder and display list | Implement the complete required profile in D08. |
| Terminal emulation | Existing `github.com/charmbracelet/x/vt` | Promote its current test-only use through the dependency decision. |

QuickJS MUST serve only the vendored diagram algorithms.
Model text MUST enter those algorithms as data.
The runtime MUST expose no filesystem, module download, network, process, DOM, or application callback API.
The distribution MUST require neither Node nor a JavaScript package manager.

The [current web renderers](../../../web/src/lib/diagrams.ts) and [package pins](../../../web/package.json) define the initial diagram baseline.
The implementation MUST pin exact engine artifacts and record their licenses and hashes.
It MUST update the native adapters and conformance corpus whenever an engine pin changes.

## D03. Preserve document identity

Each committed document MUST carry endpoint, thread, turn, and item identifiers, a monotonic revision, and immutable source bytes.
Equal item identifiers from different turns or endpoints MUST identify different documents.
Draft documents MUST use a separate local identity until the server assigns their committed identity.
Each block MUST carry a stable identity, source span, semantic kind, and rendered text projection.
Every projected text range MUST map back to its source span.

Finished block keys MUST include their document identity, source span, kind, and source hash.
The growing tail MUST retain a provisional key until its block closes.
Publishing its final key MUST atomically remap selection, focus, scroll, and pending renderer references.

The model MUST distinguish paragraphs, headings, lists, quotations, code, tables, figures, math, notices, and diff sections.
It MUST retain links, emphasis, language tags, table alignment, list starts, and task states as typed data.
UI widgets MUST NOT own the authoritative document text.

Selection endpoints MUST use document identity, stable block identity, byte offset, and affinity.
Byte offsets MUST lie on valid UTF-8 boundaries.
User movement and deletion MUST respect extended grapheme clusters.
Mappings MUST distinguish UTF-8 bytes, Unicode scalars, grapheme boundaries, and UTF-16 code units.
The selected grapheme algorithm and checked conversions follow [Input text](../../dal/spec/input-text.md).
Display normalization, fallback glyphs, and escaped controls MUST NOT rewrite immutable source or become editing coordinates.
An appended combining mark, joiner, or regional indicator MUST trigger the boundary repair that the shared contract requires.

Append-only changes MUST preserve existing anchors outside required grapheme repair at the growing tail.
Repair at that tail MUST follow each anchor's affinity and MUST NOT select unrelated text.
A replacement MUST map surviving spans before publishing the new revision.
An anchor inside removed text MUST move to the nearest surviving boundary with its original affinity.
Selection repair MUST NOT select unrelated text.

Stale parsing, highlighting, image, and figure results MUST be discarded by document revision and content key.
Results from another thread MUST never change the current document.

Status: `internal/dalgona/gui/document` keeps immutable source, revisions, block keys, and positions with an affinity.
Each run in a projection maps to its source span.
Tail blocks keep a provisional key until they close.
`State.Publish` remaps the selection, focus, scroll anchor, and pending renderer keys in one step.
Grapheme boundaries and unit conversions come from `internal/dal/inputtext`.

## D04. Render Markdown and GFM

The parser MUST support CommonMark and the GFM features listed below.
The native renderer MUST use the [web Markdown contract](web.md#markdown) as its presentation baseline.

| Construct | Required behavior |
|---|---|
| Paragraphs and breaks | Preserve paragraph boundaries, hard breaks, soft breaks, escapes, entities, and inline code. |
| Headings | Preserve all six source levels. Map their visual hierarchy through the shared design tokens. |
| Emphasis | Draw emphasis, strong emphasis, and strikethrough without losing selection or link ranges. |
| Lists | Support ordered starts, nested lists, loose lists, task states, and continuation paragraphs. |
| Quotations | Support nested quotations and their block children. |
| Rules | Render a semantic separator using the shared line role. |
| Links | Support explicit links, reference links, and GFM autolinks. |
| Tables | Preserve header cells, alignment, escaped pipes, inline content, and accessible row and column relationships. |
| Fences | Support backtick and tilde fences, language aliases, partial fences, and arbitrary literal content. |
| Raw HTML | Display literal source. Never execute or interpret it as interface markup. |
| Markdown images | Preserve the web rule `image: <alternative text>`. Do not fetch arbitrary image URLs. |
| Attachments | Render actual attached image bytes through D10. They are separate from Markdown image links. |

Task checkboxes in assistant text MUST remain read-only unless a separate documented action owns their mutation.
Link activation MUST require a user gesture.
Only `https`, `http`, and `mailto` links MAY reach the platform opener.
Malformed links MUST remain selectable text with a visible explanation when activated.

Parsing MUST operate on the smallest invalidated region that preserves correct Markdown interpretation.
Reference definitions can affect earlier blocks.
The parser MUST track these dependencies and invalidate affected blocks when a definition changes.
It MUST NOT assume that only the last paragraph can change.

An open code or diagram fence MUST show its growing source immediately.
Closing a valid diagram fence MUST replace that presentation with its figure when compilation completes.
An incomplete fragment MUST not erase the last valid rendered revision.

Status: `internal/dalgona/gui/document` builds typed blocks from the goldmark GFM tree without the HTML renderer.
A stream parses only its tail region again.
A changed link definition parses the earlier regions that mention its label.
The package also scans math delimiters, and each formula projects as one atomic run whose source span includes its delimiters.
Markdown is limited to 4 MiB per message, 64 KiB per line, and 32 nesting levels.
Content above a limit shows as literal source with a diagnostic.

## D05. Implement real text selection

The transcript MUST provide one document selection across paragraph, list, table, code, diff, and figure boundaries.
Independent `widget.Selectable` instances MUST NOT define the complete selection model.
Gio's [`Selectable.SetText`](https://pkg.go.dev/gioui.org/widget#Selectable.SetText) clears its selection when text changes.
The document model MUST preserve selection independently during streaming.

The user MUST be able to select with dragging, Shift-click, keyboard extension, word movement, line movement, and document movement.
Double-click MUST select a word.
Triple-click MUST select the visual line.
Dragging outside the viewport MUST scroll at a bounded speed without losing the anchor.

Hit testing MUST use shaped glyph clusters and line geometry.
Its result MUST resolve to a source grapheme boundary with explicit affinity under the shared input contract.
It MUST handle ligatures, combining marks, Hangul, emoji sequences, bidirectional text, tabs, and proportional fonts.
The renderer MUST NOT estimate hit positions from byte count or average glyph width.

The copy command MUST produce the document's plain text projection.
It MUST omit decorative bullets, line numbers, nontext interface controls, and diff gutters.
Unicode formatting scalars inside selected source MUST remain intact under the shared input contract.
Code copying MUST preserve source whitespace and source line endings.
Table copying MUST use tab-separated cells and newline-separated rows.
Figure copying within a mixed selection MUST use its caption followed by its source.
Math copying within a mixed selection MUST use its original TeX source.

Generated paragraph, list, and table separators MUST use LF in the plain-text projection.
Copy code, Copy patch, Copy Markdown, and source exports MUST preserve their original source line endings.
Only the operating-system clipboard format adapter MAY perform required native line-ending conversion.
That conversion MUST NOT modify stored source or exported files.

Each code block MUST also expose Copy code.
Each document MUST expose Copy Markdown and Save Markdown through its action menu.
Each diff MUST expose Copy patch separately from copying selected source lines.
The clipboard adapter MUST write text only after the complete requested content has been obtained.
It MUST never silently copy only the currently mounted rows.

Selection MUST survive new output, syntax highlighting, theme changes, font loading, window resizing, and virtualization.
Find MUST search the complete selected document scope, including unloaded pages.
It MUST identify the scope and result count before moving focus to a match.

Status: `internal/dalgona/gui/document` implements one selection across blocks and documents with grapheme, word, and block movement.
Copy uses the plain text projection, and `Markdown` returns the exact source slice.
Copy and Find page unloaded documents and return an error instead of partial text.
Copy is limited to 32 MiB.
A `gui` build shapes rows with Gio. It breaks lines only where the Gio shaper's UAX 14 line breaking allows. It uses each style's own glyph advances.
Run direction comes from one shaper pass over the whole row, so the visual order follows the shaper's bidirectional runs across style boundaries.
The shaper reorders whole runs. A number inside right-to-left text in a left-to-right paragraph keeps the paragraph order instead of the UAX 9 level 2 order.
Hit testing, carets with affinity, and selection highlights follow the shaped visual lines.
An inline formula reserves the box that `Typography.Inline` measures, and it breaks, hits, moves, and copies as one unit.
`Viewport.LineUp` and `Viewport.LineDown` move by visual line and keep the horizontal goal across rows and documents.
`Viewport.LineStart` and `Viewport.LineEnd` stop at the visual line edges, and `Document.LineSelection` selects the visual line for a triple-click.
`CopyText`, `CopyMarkdown`, and `CopySource` obtain the complete content before they write it through the platform clipboard adapter, which refuses NUL and text above 32 MiB.

## D06. Virtualize documents without losing position

The client MUST virtualize blocks, code lines, diff lines, and large table rows.
One large message MUST NOT become one indivisible list item.
The height index MUST support locating a block by document offset and updating its measured height.

Gio's [`layout.List`](https://pkg.go.dev/gioui.org/layout#List) supplies visible-child layout.
The document package MUST additionally own stable anchors, nested block virtualization, and the text caches.
It MUST lay out the visible region plus at most one viewport of overscan in each direction.

The scroll anchor MUST identify the first visible text boundary and its vertical displacement.
Reflow MUST restore that anchor before publishing the next frame.
Deferred font, figure, and image results MUST preserve the reader's anchor within one logical pixel.
An explicit jump or selection movement MAY change it.

Following output MUST use the web contract's 32-pixel threshold, expressed in logical pixels.
New output MUST NOT count as a user scroll away from the end.
Jump to latest MUST preserve its new-item count and keyboard behavior from the [component specification](native-components.md).

Cache keys MUST include source revision, width, scale, font digest, shaping version, and relevant theme roles.
Color-only changes MUST reuse geometry where the geometry is unchanged.
Eviction MUST discard derived data before authoritative source.
Source pages MUST remain recoverable through the client protocol.

| Cache or bound | Required ceiling |
|---|---|
| Resident document source pages | 32 MiB per window |
| Shaped text and geometry | 64 MiB per window |
| Decoded CPU pixels | One shared 64 MiB application LRU for all windows, figures, images, glyph tiles, and thumbnails |
| Figure GPU textures | 64 MiB per window |
| Pending figure jobs | Eight per window, including the running job |

Selection, Find, and export MUST page source as needed.
They MUST report cancellation and source retrieval failures without truncating successful-looking output.
A selection across rows MUST include each row's complete selected text projection, including nonresident prefixes.
A complete cached tail MUST NOT stand in for a complete source.
Copy MUST read cold rows through their captured source references without loading the whole selection into the pane cache.
A cold structured row MUST reserve its source and projection backings against the same window budget.
Its raw source MUST NOT exceed 8 MiB, and occupied leases MAY reduce that limit.
A source-budget failure MUST leave the clipboard unchanged.
Structured formatting MUST check its expanded extent and reserve all retained source backings before creating the expanded output.
It MUST reserve five times the formatted extent against the window budget.
The five backings cover the formatter buffer with its spare quarter, the formatted string, the combined row and the document source.
This requirement MUST apply to visible rows, cold selection copies, and Full text.
The view MUST show a projection-budget failure without offering a daemon read retry.
An independent action with complete captured raw source MUST remain available when only its formatted projection fails.
Windows MUST share immutable pixel allocations through the application LRU rather than copy them into separate caches.
Full-image previews MUST use bounded viewport tiles and release obsolete tiles before admitting replacements.
These ceilings are design limits, not measured performance claims.

The user MUST be able to fold a code block of ten or more lines.
The user MUST be able to fold a diff file section of more than one line.
The user MUST be able to fold a top-level quotation.
A folded block MUST show one header row.
The header row MUST show the first row of the block and the count of hidden rows.
The header row MUST give the user an action to expand the block.
The document scope MUST keep the fold state, and the fold MUST stay when rows are added to or removed from the transcript.
A fold MUST follow its block key when a stream closes the block.
A fold toggle MUST keep the reader's anchor.
If the anchor is in the folded rows, the anchor MUST move to the header row at or below the top of the viewport.
Line movement MUST skip folded rows and MUST go from the header row to the first row after the fold.
Copy, Find, and export MUST use the full source of a folded block.

Status: `internal/dalgona/gui/document` implements the height index, the overscan window, the scroll anchor, following, and a geometry cache with a byte budget.
The anchor keeps its source position when content arrives above it.
Only `gui` code measures real row heights.
Code blocks of ten or more lines, diff file sections with more than one line, and top-level quotations fold to one header row.
Toggling a fold keeps the reader's anchor, and an anchor inside the fold moves to the fold header at or below the viewport top.
Folds follow their block keys when a stream closes, and line movement skips folded rows.

## D07. Draw code and diffs

The highlighter MUST support every language and alias registered by the [web renderer](../../../web/src/lib/markdown.ts).
That baseline includes Bash, CSS, diff, Dockerfile, Go, INI, JavaScript, JSON, Markdown, Python, Rust, SQL, TypeScript, XML, and YAML.
Aliases MUST include `sh`, `shell`, `toml`, `html`, `ts`, and `js`.
Unknown languages MUST render faithful plain code with their language label.

Highlighting MUST emit typed source ranges.
Every token MUST preserve its original bytes.
Concatenating token source ranges MUST reproduce the input exactly.
Token colors MUST map to registered design roles.
The implementation MUST add no unregistered syntax palette.

Large blocks MUST show text before highlighting finishes.
Highlighting MUST run outside the frame handler with cancellation and revision checks.
Its hard deadline MUST use the renderer child rather than abandon an uninterruptible lexer goroutine.
A lexer exceeding its budget MUST leave complete plain code and a named highlighting error.
It MUST NOT block scrolling or omit source lines.

Diff presentation MUST preserve file names, operation kind, hunk headers, old and new line numbers, and changed text.
It MUST distinguish add, delete, modify, rename, binary, missing blob, and omitted diff states.
The client MUST preserve `diffOmitted` from the [protocol](../../dal/spec/protocol.md).
An omitted diff MUST never appear as an empty change.

Unified view is the default.
Side-by-side view MUST align corresponding hunks and retain independent horizontal positions.
Both views MUST provide file navigation, hunk navigation, selection, Copy patch, and Open file.
Opening a remote file MUST use the daemon's file contract rather than a matching local pathname.
An approval decision MUST answer its corresponding pending server request through the approval component.
Reading a review result MUST not create an approval request or imply approval.
Expanding or copying a diff MUST never approve an edit.

Status: `internal/dalgona/gui/document` splits a patch into one block per file section.
A section starts at a `diff` command or `diff --git` header, a unified `---` header, or a standalone `Binary files` line.
The splitter counts hunk lines, so a changed line that starts with `---` or `+++` stays inside its hunk.

## D08. Render diagrams and static SVG

The client MUST render Mermaid, D2, nomnoml, and SVG fences as native figures.
The language label, source inspector, Copy source, Copy image, Save SVG, Save PNG, zoom, and fit controls MUST remain available.
These actions MUST have keyboard and accessibility equivalents.

The initial Mermaid dialect MUST include every construct accepted by the web client's pinned `beautiful-mermaid` version.
Its baseline diagram families are flowchart, state, sequence, class, entity relationship, and XY chart.
The implementation MUST not describe that dialect as the entire Mermaid language.
Each accepted family MUST have native conformance fixtures covering nesting, labels, directions, edges, and theme changes.

D2 MUST support the bundled Dagre grammar, containers, connections, labels, shapes, and styles without external plugins.
Nomnoml MUST support the pinned parser's nodes, relations, compartments, directives, and styles.
Neither compiler MAY resolve external files or URLs.
An import or resource directive MUST produce a precise error at its source span.

The figure decoder MUST convert engine SVG into an immutable display list.
It MUST preserve vector paths, text labels, group transforms, clipping, opacity, and paint order.
The display list MUST contain finite coordinates and bounded counts.
It MUST carry semantic labels separately from glyph outlines.

The static SVG profile MUST include the following features:

| Area | Required behavior |
|---|---|
| Geometry | `svg`, `g`, `path`, `rect`, `circle`, `ellipse`, `line`, `polyline`, and `polygon`. |
| Paths | Absolute and relative moves, lines, cubic curves, quadratic curves, arcs, and close operations. |
| Coordinates | `viewBox`, `preserveAspectRatio`, nested transforms, and bounded lengths in numbers, pixels, points, picas, inches, centimeters, millimeters, ems, exs, and percentages. |
| Paint | Solid fills, strokes, fill rules, opacity, joins, caps, dash arrays, and current color. |
| Definitions | Local identifiers, bounded `use`, markers, symbols, clipping paths, and masks. |
| Gradients | Linear and radial gradients with stop opacity and their transforms. |
| Text | `text`, `tspan`, anchors, baselines, font weights, and shaped multiline labels. |
| Generator styles | Resolve every style emitted by the pinned engines into explicit display-list properties. |
| Generator effects | Implement each emitted static filter through bounded native raster tiles. |

The renderer owner MUST maintain an engine-output inventory and fixtures for every emitted feature.
An engine feature missing from the decoder is required implementation work and blocks release.
It MUST NOT become a permanent source-only fallback for a supported diagram.

Percentages MUST resolve against the corresponding viewport or object bounding box required by that SVG property.
Font-relative units MUST use the selected label face's metrics.
The decoder MUST reject non-finite transforms and coordinates beyond one million logical pixels in magnitude.

Raw SVG MUST reject scripts, event attributes, animation, `foreignObject`, external references, and resource-loading elements.
It MUST reject document types and entity declarations before decoding.
Raw styles MUST use the same bounded property allowlist as normalized engine styles.
Selectors, variables, and unsupported properties MUST produce a located error rather than partially changing meaning.
Local identifier references MUST detect cycles and enforce the expansion limit.
Authored SVG gradients are document content and MAY retain their colors.
The interface chrome MUST continue to obey the design system's prohibition on gradients.

Diagram label measurement MUST use the same bundled fonts as figure rendering.
The adapter MUST replace byte-count approximations that clip Hangul or mixed-script labels.
All supported diagrams MUST fit their declared bounds without losing labels, edges, or arrowheads.

Source inspection MUST remain a secondary view of a successfully rendered figure.
A failure MUST show its cause, source location when known, Retry, and intact source.
Retry MUST follow the worker restart policy in D11.

Status: `internal/dalgona/gui/figure` compiles D2 with the bundled dagre layout.
It refuses D2 imports, icons, and image shapes at their source spans.
It also refuses sketch mode, fill patterns, and other layout engines at their source spans.
D2 Markdown labels are parsed by the `internal/dalgona/gui/document` model and laid out as native text runs measured with the D2 Source Sans Pro faces.
`internal/dalgona/gui/figure/math` parses and lays out D2 LaTeX labels. It draws them as glyph outlines. The formula source is the semantic label.
It runs the vendored Mermaid, elkjs, entities, nomnoml, and graphre bundles in QuickJS without a host API.
The files `js/SHA256SUMS` and `js/NOTICE` pin the bytes, versions, and licenses of these bundles.
The owned decoder changes raw SVG and engine SVG into a validated display list.
The decoder covers the geometry, path, coordinate, paint, definition, gradient, and text rows of the profile.
It also resolves the engine styles and the engine shadow filter.
Gio fills paths only with the nonzero rule.
Thus even-odd fills and radial gradients become raster tiles.
Masks and clips are decomposed into exact trapezoid regions, so overlapping mask shapes, partial luminance or alpha, and `evenodd` clip children keep their meaning.
A gradient inside a mask is drawn in bands of 1/64 of its offset. At most one mask gradient may shade one area.
D2 sequence-diagram groups use multiply blending. The decoder draws the group under its backdrop. It replays the backdrop inside the group tinted by the multiply factor.
Nomnoml and D2 measure labels with the Source Sans Pro faces that the drawer receives.
The Mermaid bundle keeps its own width estimate.
A `gui` build draws the display list through `Canvas`.

## D09. Render mathematical notation

The client MUST support inline `\(...\)` and display `\[...\]` math.
It MUST also support `$...$` and `$$...$$` outside literal code.
A single-dollar opener MUST have non-whitespace content immediately after it.
A single-dollar closer MUST follow non-whitespace content and MUST NOT precede a decimal digit.
Escaped dollar signs MUST remain literal.
Delimiter parsing MUST preserve source spans and MUST not reinterpret code fences.

The following grammar defines required support:

| Group | Required constructs |
|---|---|
| Atoms | Numbers, Latin and Greek identifiers, common Unicode math symbols, grouped expressions, and escaped punctuation. |
| Scripts | Superscripts, subscripts, nested scripts, primes, operator limits, and display or inline style. |
| Fractions | `frac`, `dfrac`, `tfrac`, `binom`, and continued nested fractions. |
| Roots and fences | `sqrt`, optional root indices, `left`, `right`, `middle`, and sized delimiters. |
| Operators | `sum`, `prod`, `coprod`, `int`, `iint`, `iiint`, `oint`, `lim`, `sup`, `inf`, `max`, `min`, `operatorname`, relations, arrows, and set operators. |
| Decoration | Accents, vectors, overlines, underlines, wide hats, tildes, overbraces, and underbraces. |
| Text and alphabets | `text`, `mathrm`, `mathbf`, `mathit`, `mathsf`, `mathtt`, `mathbb`, and `mathcal`. |
| Arrays | `matrix`, `pmatrix`, `bmatrix`, `Bmatrix`, `vmatrix`, `Vmatrix`, `smallmatrix`, and `array`. |
| Alignment | `aligned`, `alignedat`, `gathered`, `cases`, `align`, `align*`, `gather`, `gather*`, `equation`, and `equation*`. |
| Labels and spacing | `tag`, `tag*`, row breaks, alignment markers, `quad`, `qquad`, `enspace`, thin spaces, negative thin spaces, and explicit bounded spacing. |

Named functions MUST include `sin`, `cos`, `tan`, `cot`, `sec`, `csc`, their hyperbolic forms, `log`, `ln`, `exp`, `det`, `gcd`, and `Pr`.
Greek commands MUST include every lowercase Greek letter and each distinct uppercase Greek math symbol.
Relations MUST include equality, inequality, approximation, ordering, membership, subset, superset, and equivalence commands.
Arrows MUST include left, right, bidirectional, double, mapping, and their long forms.
Set operators MUST include union, intersection, difference, empty set, conjunction, disjunction, and quantifiers.
Each command in these groups MUST have an explicit parser entry and a conformance fixture.
Unknown commands MUST identify their name and location without swallowing surrounding source.

The implementation MUST use an owned math AST and box layout.
It MUST implement font metrics from the [OpenType MATH table](https://learn.microsoft.com/en-us/typography/opentype/spec/math).
Fractions, scripts, radicals, large operators, accents, and stretchable delimiters MUST use those metrics.
Arbitrary glyph scaling MUST NOT replace delimiter assemblies or math-axis placement.

The distribution MUST bundle a licensed math font and its exact digest.
STIX Two Math is the selected face.
Text inside mathematics MUST use the document's matching text face and Hangul fallback.
The layout MUST preserve baseline, ascent, descent, and inline spacing.
Wide display formulas MUST scroll within their figure without widening the whole document.

Alignment environments MUST have explicit row, column, and equation-tag layout.
Missing behavior in a reused parser or font library MUST be implemented by the math owner before release.
File access, package loading, shell escapes, global macros, and arbitrary TeX programs MUST be rejected.
Formula-local definitions MUST also be rejected in this grammar.
Automatic equation numbering MUST not infer document-wide state.
Explicit tags MUST be preserved, and untagged equations MUST remain unnumbered.

Copy TeX MUST return the original formula.
Save SVG and Save PNG MUST export the rendered formula.
Accessibility MUST expose a formula role, source, and a navigable semantic expression.
Source inspection alone MUST NOT satisfy the math accessibility gate.

Status: `internal/dalgona/gui/figure/math` parses this grammar into an owned semantic tree with source spans.
It enforces the formula source, nesting, node, and array bounds in D11.
It reports each refusal with its source span.
It reads the OpenType MATH table itself.
The layout puts fractions, scripts, primes, operator limits, radicals, accents, and braces into positioned glyphs, rules, and text runs.
It also lays out stretchy delimiters, arrays, alignment environments, and tags.
A `gui` build draws these items from the glyph outlines of the math font and the document text face.
`internal/dalgona/gui/platform` embeds STIX Two Math 2.13 b171, and `MathFontData` returns it only after it checks the pinned SHA-256.
The caller passes those bytes to `ParseFont`. The caller passes Hanken Grotesk with the Pretendard fallback from `MathTextFaceData` to `NewTextFont` for text runs and missing math glyphs.
The math package does not import the platform package.
Script and scriptscript glyphs take the `ssty` alternates of the `math` script, or of the `DFLT` script when `math` has none.
An accent over a base taller than `FlattenedAccentBaseHeight` takes its `flac` form, and a wide accent keeps its size variants.
MATH device tables adjust constants, italic corrections, accent attachments, kerns, and assembly italic corrections at the rendered pixel size. A malformed device table adds nothing.
Copy TeX copies `Formula.Source`, the exact formula source, through the caller's clipboard path.
`Layout.WriteSVG` writes each glyph outline once and places each use of it, with rules and text runs as paths.
`Layout.WritePNG` rasterizes the same outlines at the chosen scale in bands of 256 rows and tiles of 256 columns.
It refuses more than 32 million pixels or 256 MiB of encoded output.
`Layout.Expression` returns math-owned nodes in reading order, each after its parent, for the caller to map into the platform semantic model.
Each node names its role, speaks its subtree, carries its source text, and has its bounds in layout pixels.
Markdown delimiter scanning remains.

## D10. Handle image attachments

The client MUST accept file selection, file drop, and clipboard image input through the [platform adapters](native-platform.md).
It MUST preserve the current limits of ten images and five MiB per encoded image.
Accepted wire formats remain PNG, JPEG, and GIF.
The aggregate image-file bytes MUST not exceed ten MiB after base64 decoding.
This aggregate counts compressed file bytes, not decoded pixels.
The complete message input array MUST remain within the current fifteen MiB JSON limit.

The client MUST inspect magic bytes and decode headers before allocating pixel storage.
Filename extensions and declared MIME types MUST NOT establish the format.
Each decoded image MUST contain at most 32 million pixels and at most 128 MiB of pixel data.
Neither dimension MAY exceed 16,384 pixels.
Dimensions, row strides, frame counts, and allocation arithmetic MUST be checked before decoding.
The renderer child MAY hold that bounded decoded image within its own process-memory limit.
The parent MUST never allocate a matching full-image buffer merely to receive it.
It MUST receive viewport tiles or downsampled previews through reply frames of at most one MiB.

Clipboard-only platform formats MUST be converted into PNG before they enter the attachment model.
The converted bytes MUST meet the same encoded-size limit.
Attachments MUST retain their original accepted wire bytes for submission.
Thumbnail generation MUST not silently replace those bytes with a lower-quality image.

The attachment component MUST show name, format, byte size, thumbnail, preview, and remove actions.
GIF previews MUST use the first frame by default and preserve the original GIF for submission.
Preview MUST respect orientation and color metadata supported by the decoder.
Full-size viewing MUST decode only the active image and release it when the viewer closes.
The parent MUST admit its tiles through the shared 64 MiB pixel LRU.
Pan and zoom MUST request replacement tiles without duplicating the full image.

The client MUST reject image input when the model has no image capability.
An unreadable, oversized, or invalid image MUST leave the existing draft and attachments unchanged.
Adding several files MUST report individual failures without losing accepted files.
A send failure MUST preserve every attachment for an explicit retry.

Images restored from conversation history MUST use the authorized image-read method in [native protocol](native-protocol.md).
Their identity MUST include endpoint, thread, turn, item, image index, and content digest.
The client MUST verify the complete received digest before presenting decoded pixels.
It MUST not reinterpret a remote path or `dalgona-blob:` value as permission to read a local file.
Historic images MUST remain viewable after the selected model changes or the original local file disappears.
Copy image and Save image MUST work from authorized stored bytes without changing the conversation.

Image decoding MUST use the renderer child and the D11 deadlines.
No image decoder MAY block the frame handler.
Screen readers MUST receive meaningful names, dimensions, attachment order, and control labels.

Status: `internal/dalgona/gui/figure` finds the image format from magic bytes and reads PNG, JPEG, and GIF headers.
It applies the size, side, pixel, and pixel-byte limits before a decoder allocates pixels.
The renderer child decodes the first frame, applies the EXIF orientation, and returns preview tiles.
The `Supervisor` admits every received preview tile into its `PixelCache`. The `PixelCache` is a 64 MiB least-recently-used cache. It is keyed by the image SHA-256 digest, the preview side, and the tile origin.
An image outcome carries only that key. `Release` drops the tiles of an image when its viewer closes.

## D11. Bound renderer execution

The GUI executable MUST provide a private renderer-child entry point.
The parent MUST launch it before any GUI, daemon, or credential initialization occurs in the child.
Invocation without valid inherited worker pipes MUST fail before that initialization.
The child MUST receive framed requests over inherited pipes and return framed typed results.
It MUST receive no daemon credentials, journal handles, terminal descriptors, or environment secrets.

The worker protocol MUST use a four-byte little-endian frame length followed by a typed envelope and an optional binary payload.
The envelope MUST use the repository's JSON codec and contain its own bounded byte length.
An envelope MUST contain at most 4,096 bytes.
Binary pixels and source bytes MUST not be base64-encoded inside this private protocol.

| Message | Required fields and meaning |
|---|---|
| `hello` | Protocol version, engine-manifest digest, font-manifest digest, and the launch nonce supplied through the inherited control pipe. |
| `render` | Job ID, document identity, revision, content key, kind, source length, theme roles, scale, viewport, and deadline. |
| `image` | Job ID, content key, validated format, encoded byte length, requested viewport, and requested scale. |
| `highlight` | Job ID, document identity, revision, language, source length, and content key. |
| `result` | Job ID, content key, result kind, byte budget, bounds, baseline, semantic counts, and known totals when available. |
| `chunk` | Job ID, zero-based chunk number, payload length, pixel format or display-list kind, and tile bounds when applicable. |
| `complete` | Job ID, total chunk count, total byte count, and digest of the ordered result bytes. |
| `error` | Job ID, typed failure code, bounded message, and optional source span. |

Request and result payload lengths MUST be checked before allocation or reading.
Complete semantic labels and expression trees MUST travel in typed result chunks, not the bounded envelope.
Their bytes count toward the serialized result limit and final digest.
Valid figures with more than 4,096 bytes of labels MUST preserve every label within that result budget.
The parent MUST compare the hello manifests with its own embedded manifests before dispatch.
A version, manifest, or nonce mismatch MUST terminate the child.
The nonce detects a mismatched channel and MUST not be described as a process sandbox.

The parent MUST commit a result only after its completion totals and digest have been verified.
It MUST reject duplicate, missing, reordered, overlapping, and out-of-bounds chunks.
Partially received data MUST remain invisible and be released after cancellation.
Streaming exports MAY write verified chunks to their temporary file before completion.
They MUST enforce the declared byte budget and rename only after completion verifies the final totals and digest.
Error codes MUST distinguish invalid input, forbidden resource, limit exceeded, timeout, cancellation, engine failure, and protocol failure.

The parent MUST validate response length before allocation.
It MUST validate the content key, job identifier, dimensions, finite coordinates, operation counts, and font identifiers before accepting a result.
A malformed result MUST terminate the child and become a visible rendering failure.

| Resource | Required bound |
|---|---|
| Active renderer children | One per GUI process |
| Diagram, SVG, or math source | 64 KiB per figure |
| Code-highlighting source | Four MiB per job |
| Diagram graph | 1,000 nodes and 2,000 edges |
| Formula nesting | 64 levels |
| Formula nodes | 10,000 nodes |
| Formula arrays | 256 rows, 256 columns, and 4,096 cells |
| SVG nesting | 64 levels |
| Expanded SVG elements | 50,000 elements |
| Display-list operations | 200,000 operations |
| Serialized display-list result | 16 MiB in bounded chunks |
| Pixel reply frame | One MiB including its framing metadata |
| One raster tile | 256 by 256 pixels and 256 KiB |
| Raster figure export | 32 million pixels and 256 MiB of streamed encoded output |
| QuickJS heap | 128 MiB |
| QuickJS call depth | 2,048 VM stack slots |
| Render deadline | Two seconds from dispatch |
| Parent termination allowance | 250 milliseconds after the deadline |
| Child resident-memory stop threshold | 512 MiB, sampled at least every 50 milliseconds |
| Automatic restart | One restart after a child failure, with no automatic replay of the failed input |

The memory threshold is a termination trigger, not a promise that transient allocation cannot exceed it.
The parent MUST additionally enforce exact input, output, pixel, and geometry bounds.
Larger pixel results MUST use bounded tile frames without exceeding the shared CPU pixel cache.
Platform process limits MAY strengthen these bounds.
They MUST NOT be described as an operating-system sandbox.

The parent MUST terminate and reap the child after a deadline or resource breach.
Cancellation MUST terminate a running job when its engine cannot stop cooperatively.
A pending obsolete job MUST be discarded without launching it.
Queue saturation MUST preserve visible source and display `Waiting to render`.
Visible figures MUST take priority over overscan figures.

Failed input MUST not trigger a restart loop.
The user MAY retry that figure once after changing the source or explicitly choosing Retry.
Three child failures within sixty seconds MUST pause automatic rendering and show Resume rendering.
Text, input, approvals, and terminal operation MUST remain available during this state.

Status: `internal/dalgona/gui/figure` supplies the private renderer child as `ServeChild` behind `ChildArgument`.
The parent is `Supervisor`.
It checks the frames, the nonce, the manifests, the chunk digests, and each result.
It stops the child at the deadline, keeps one queue of eight jobs, and pauses after three failures.
It permits one retry of failed input.
On Linux, the parent samples the resident memory of the child from `/proc`.
The macOS sampler reads `proc_pidinfo` task information. The Windows sampler reads `GetProcessMemoryInfo`. Both are compile-checked and have not yet run on those hosts.
The child also stops when its Go memory is more than the threshold.
The child loads a script engine on the first job of that kind.
A warm-up request loads the engine, and the job deadline does not include this time.

## D12. Export complete content

Save Markdown MUST export the selected document scope with original fences and attachment references.
Save source MUST export the exact selected code, diagram, or formula source.
Save SVG MUST export a normalized static figure with embedded font outlines or complete portable glyph definitions.
Save PNG MUST use the same figure geometry at the user-selected scale within D11 bounds.
Image export MUST stream encoded bytes or tile rows through bounded buffers.
It MUST not stitch a full 128 MiB image inside the GUI process.
Supported full-size images MUST remain exportable despite the smaller presentation cache.

Every export MUST identify its scope and destination before writing.
The save dialog owns overwrite confirmation.
The writer MUST finish a temporary sibling file before replacing the destination.
Cancellation or failure MUST preserve an existing destination.
An export MUST report a failed source page or figure rather than silently omitting it.

The client MUST never open a network URL merely to complete an export.
Exported metadata MUST omit local credentials, private worker paths, and unrelated thread content.

Status: `internal/dalgona/gui/document` streams Save Markdown from loaded and paged source.
`SaveFile` writes, syncs, and closes a temporary sibling file before it replaces the destination, and then it syncs the destination folder outside Windows.
It keeps the old destination when an export fails or stops before the replacement.
A failed folder sync after the replacement returns an error that says the destination already holds the new content.
`WriteSource` writes the exact code, diagram, or formula source.
Save SVG and Save PNG belong to the figure package, and `internal/dalgona/gui/figure/math` writes both for a laid-out formula.

## D13. Distinguish terminal state from command logs

Command cards MUST render recorded command output as document content.
Their output MUST NOT accept terminal input or claim an interactive session.
The current `command/exec` protocol explicitly lacks TTY, input streaming, and resize support.

The Terminal component MUST use the real PTY contract in [native protocol](native-protocol.md).
It MUST maintain terminal state from ordered byte output and resize records through the selected VT emulator.
It MUST never flatten that output into append-only paragraphs before parsing terminal controls.
Resize records MUST take effect at their declared byte offsets and ordinals for writer and read-only views alike.
Checkpoints MUST restore both the byte and resize cursors. A resize reply MUST NOT bypass unread output.
An attached reader MUST refresh a lost replay position through `dalgona/terminal/checkpoint/open` while retaining its attachment identity.

The terminal MUST support cursor movement, screen erase, alternate screen, scroll regions, attributes, Unicode cells, and terminal replies.
It MUST support bracketed paste, application cursor keys, mouse reporting, focus reporting, and resize.
User input MUST travel through the authenticated session owner and its writer lease.
The daemon's authoritative VT processor alone MUST answer automatic terminal queries while consuming live output.
GUI emulators MUST suppress those replies during rendering, checkpoint import, and replay.
Reader attachments MUST never generate terminal replies or acquire input authority from output.

The renderer MUST draw changed cells and preserve wide-cell and combining-sequence boundaries.
Selection MUST support character, word, line, and rectangular modes.
Copy MUST remove terminal control sequences and preserve the selected visible text.
Search MUST include retained scrollback and identify its bounded scope.

The terminal MUST retain at most 10,000 scrollback lines and one MiB of decoded scrollback per session.
It MUST discard the oldest lines first and show the retained-history boundary.
Output transport gaps MUST be visible and recovered through the protocol's snapshot or replay contract.
The client MUST never continue a screen after silently dropping escape-sequence bytes.
Stop MUST preserve the reader until final byte and resize cursors are consumed, the user explicitly releases it, or retention expires.
An expired final-output lease or truncated drain MUST remain visible and MUST NOT be reported as complete output.

Operating-system clipboard requests from terminal output MUST require an explicit user action.
Terminal output MUST not launch programs, open URLs, or change clipboard contents by itself.
Unknown terminal sequences MUST be ignored by the bounded parser without affecting input ownership.

Status: `internal/dalgona/vtstate` holds the shared VT processor and the `VTStateV1` checkpoint adapter.
`internal/dalgona/gui/terminal` holds replica replay, selection, search, input encoding, and cell drawing.
Independent `VTStateV1` continuation fixtures cover every supported parser state with hand-derived resume oracles under `internal/dalgona/vtstate/testdata/fixtures/` and negative malformed-state cases under `testdata/invalid/`.
The Terminal component, its protocol client, and real PTY evidence remain open.

## D14. Prove document completeness

The document owner MUST supply the following evidence to [native verification](native-verification.md).
Each gate MUST identify the exact corpus, runtime versions, and platform used.

| Gate | Required evidence |
|---|---|
| Markdown | CommonMark and GFM semantic cases, plus the web renderer's accepted fixtures, pass through native AST and layout. |
| Selection | Cross-block copy exactly matches an independent expected text projection through streaming, reflow, and virtualization. |
| Unicode | Hangul, combining marks, emoji sequences, RTL runs, ligatures, and UTF-16 boundaries preserve text and selection. |
| Code | The registered language corpus highlights without altering or losing source bytes. |
| Diffs | Unified and split views preserve patch meaning, missing-blob states, and exact copied text. |
| Diagrams | Every pinned engine family and emitted SVG feature renders correctly at all supported scales. |
| Math | Every required command group and environment renders, copies, exports, and exposes its semantic expression. |
| Images | Valid formats preview and send unchanged. Malformed headers and decompression cases fail within resource bounds. |
| Worker | Forced hangs, crashes, malformed frames, and resource breaches leave no child or goroutine leak. |
| Terminal | Independent VT fixtures and real shell sessions prove screen state, input, resizing, alternate screen, and replay recovery. |

Stress fixtures MUST include 10,000 messages, a 100,000-line code block, a 20,000-row table, and a 100,000-line diff.
Fixtures MUST include selection spanning unloaded pages and an active stream while earlier content is selected.
They MUST prove bounded resident caches and stable scroll anchors.
The [verification specification](native-verification.md) owns timing and whole-process memory targets.

## Primary sources

- [Gio project and architecture](https://gioui.org/).
- [The requested Gio Context7 reference](https://context7.com/git_git_sr_ht/~eliasnaur_gio).
- [Gio text widgets](https://pkg.go.dev/gioui.org/widget).
- [Gio list layout](https://pkg.go.dev/gioui.org/layout#List).
- [Goldmark v1 source](https://github.com/yuin/goldmark/tree/v1.8.2).
- [Chroma v2 source](https://github.com/alecthomas/chroma/tree/v2.27.0).
- [Beautiful Mermaid's pinned native-independent API](https://github.com/lukilabs/beautiful-mermaid/blob/v1.1.3/src/index.ts).
- [D2 compiler](https://github.com/terrastruct/d2/tree/v0.7.2/d2lib).
- [Nomnoml's source and SVG API](https://github.com/skanaar/nomnoml).
- [Pure Go QuickJS resource APIs](https://pkg.go.dev/modernc.org/quickjs).
- [OpenType mathematical layout metrics](https://learn.microsoft.com/en-us/typography/opentype/spec/math).
- [STIX fonts and licenses](https://github.com/stipub/stixfonts).
- [The Go VT emulator](https://pkg.go.dev/github.com/charmbracelet/x/vt).

Documentation examples MUST be checked against the selected upstream source revision.
An indexed example MUST not override the current API contract.
