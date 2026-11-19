#### Web step 15. Markdown and code rendering

- Dependencies. `"marked": "18.2.0"`, `"dompurify": "3.4.17"`, `"highlight.js": "11.12.0"`. Add rows to the `## Grounded pins` table (step 1).
- `web/src/lib/markdown.ts`
  - Imports `hljs from "highlight.js/lib/core"` and registers languages from `highlight.js/lib/languages/<name>`: `bash`, `diff`, `go`, `javascript`, `typescript`, `json`, `python`, `rust`, `yaml`, `ini`, `markdown`, `css`, `xml`, `sql`, `dockerfile`. Aliases: `sh`, `shell` → `bash`; `toml` → `ini`; `html` → `xml`; `ts` → `typescript`; `js` → `javascript`.
  - A `new Marked({ gfm: true, breaks: false, async: false })` instance with renderer overrides:
    - `html` returns the escaped token text.
    - `heading` renders `h${Math.min(depth + 2, 6)}`.
    - `code` returns `<pre><code class="hljs language-${lang}">` with highlighted HTML when the language is registered, else escaped text.
    - `image` returns `<span class="md-image">image: ${escaped alt}</span>`.
    - `link` returns `<a href>` only for `/^(https?:|mailto:)/i`, else the link text.
  - `export function renderMarkdown(text: string): string` returns `DOMPurify.sanitize(html, config)`. The config:
    - `ALLOWED_TAGS`: `p`, `br`, `hr`, `h3`, `h4`, `h5`, `h6`, `strong`, `em`, `del`, `code`, `pre`, `blockquote`, `ul`, `ol`, `li`, `a`, `table`, `thead`, `tbody`, `tr`, `th`, `td`, `span`
    - `ALLOWED_ATTR`: `href`, `title`, `class`, `start`, `align`
    - `ALLOWED_URI_REGEXP: /^(?:https?:|mailto:)/i`, `ALLOW_DATA_ATTR: false`
  - Hook `afterSanitizeAttributes`:
    - Every `a` gets `rel="noopener noreferrer nofollow"` and `target="_blank"`.
    - On `class`, keep only tokens matching `/^(hljs(-[a-z]+)*|language-[a-z0-9+#-]+|md-image)$/`.
- `web/src/lib/components/Markdown.svelte`, props `{ text: string; streaming: boolean }`.
  - When not streaming, render at once.
  - While streaming, coalesce renders to one per `requestAnimationFrame`.
  - Output goes through `{@html html}`, the only `{@html}` in `web/src`.
  - Code styles in `app.css`: `.hljs-keyword` uses `accentText`, `.hljs-string` uses `addedText`, `.hljs-number` uses `warningText`, `.hljs-title` and `.hljs-type` use `text`, and `.hljs-comment` and `.hljs-meta` use `dim`. No other colour exists (Design record, Colour). Code blocks use `background:var(--tone)` and `white-space:pre-wrap`.
- Tests. `web/src/lib/markdown.dom.test.ts`, describe `sanitising rule`. Vectors come from the OWASP XSS Filter Evasion Cheat Sheet:
  - `<script>alert(1)</script>`
  - `<img src=x onerror=alert(1)>`
  - `[x](javascript:alert(1))`
  - `<svg onload=alert(1)>`
  - `<a href="data:text/html,x">x</a>`
  - `<iframe src=//x>`
  - `![a](http://x/y.png)`
  - Expect no `script`, `img`, `svg` or `iframe` element, no attribute whose name starts with `on`, and every `href` matching `/^(https?:|mailto:)/`.
  - Positive cases: fenced `go` code gives `pre > code.hljs` containing `span.hljs-keyword`; `# T` gives `h3`; a link gets `rel` and `target`.
  - Fails before: the module is absent.
- Spec (`docs/spec/web.md`). New `### Markdown`: the renderer, heading shift, sanitising rule, link rule, no images, and the language list.
- Lands after: Steps 1 and 8 (the table and the colour tokens).

Anchor: Tony Hoare, Claude Shannon.

