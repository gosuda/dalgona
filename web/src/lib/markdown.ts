import DOMPurify, { type Config } from "dompurify";
import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import dockerfile from "highlight.js/lib/languages/dockerfile";
import go from "highlight.js/lib/languages/go";
import ini from "highlight.js/lib/languages/ini";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import markdownLanguage from "highlight.js/lib/languages/markdown";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";
import { Marked, type RendererObject } from "marked";

hljs.registerLanguage("bash", bash);
hljs.registerLanguage("css", css);
hljs.registerLanguage("diff", diff);
hljs.registerLanguage("dockerfile", dockerfile);
hljs.registerLanguage("go", go);
hljs.registerLanguage("ini", ini);
hljs.registerLanguage("javascript", javascript);
hljs.registerLanguage("json", json);
hljs.registerLanguage("markdown", markdownLanguage);
hljs.registerLanguage("python", python);
hljs.registerLanguage("rust", rust);
hljs.registerLanguage("sql", sql);
hljs.registerLanguage("typescript", typescript);
hljs.registerLanguage("xml", xml);
hljs.registerLanguage("yaml", yaml);
hljs.registerAliases(["sh", "shell"], { languageName: "bash" });
hljs.registerAliases(["toml"], { languageName: "ini" });
hljs.registerAliases(["html"], { languageName: "xml" });
hljs.registerAliases(["ts"], { languageName: "typescript" });
hljs.registerAliases(["js"], { languageName: "javascript" });

const SAFE_LINK = /^(?:https?:|mailto:)/i;
const SAFE_CLASS = /^(hljs(-[a-z]+)*|language-[a-z0-9+#-]+|md-image)$/;

function escapeHtml(text: string): string {
	return text
		.replaceAll("&", "&amp;")
		.replaceAll("<", "&lt;")
		.replaceAll(">", "&gt;")
		.replaceAll('"', "&quot;")
		.replaceAll("'", "&#39;");
}

const renderer: RendererObject = {
	heading({ tokens, depth }) {
		const level = Math.min(depth + 2, 6);
		return `<h${level}>${this.parser.parseInline(tokens)}</h${level}>\n`;
	},
	code({ text, lang }) {
		const name = (lang ?? "").trim().split(/\s+/, 1)[0]?.toLowerCase() ?? "";
		const highlighted = name && hljs.getLanguage(name);
		const body = highlighted
			? hljs.highlight(text, { language: name, ignoreIllegals: true }).value
			: escapeHtml(text);
		const classes = name ? `hljs language-${escapeHtml(name)}` : "hljs";
		return `<pre><code class="${classes}">${body}</code></pre>\n`;
	},
	html({ text }) {
		return escapeHtml(text);
	},
	link({ href, title, tokens }) {
		const label = this.parser.parseInline(tokens);
		if (!SAFE_LINK.test(href)) return label;
		const titleAttr = title ? ` title="${escapeHtml(title)}"` : "";
		return `<a href="${escapeHtml(href)}"${titleAttr}>${label}</a>`;
	},
	image({ text }) {
		return `<span class="md-image">image: ${escapeHtml(text)}</span>`;
	},
};

const markdown = new Marked({ gfm: true, breaks: false, renderer });

const sanitizeConfig: Config = {
	ALLOWED_TAGS: [
		"p",
		"br",
		"hr",
		"h3",
		"h4",
		"h5",
		"h6",
		"strong",
		"em",
		"del",
		"code",
		"pre",
		"blockquote",
		"ul",
		"ol",
		"li",
		"a",
		"table",
		"thead",
		"tbody",
		"tr",
		"th",
		"td",
		"span",
	],
	ALLOWED_ATTR: ["href", "title", "class", "start", "align"],
	ALLOWED_URI_REGEXP: SAFE_LINK,
	ALLOW_DATA_ATTR: false,
};

DOMPurify.addHook("afterSanitizeAttributes", (node) => {
	if (node.localName === "a") {
		node.setAttribute("rel", "noopener noreferrer nofollow");
		node.setAttribute("target", "_blank");
	}
	if (!node.hasAttribute("class")) return;
	const kept = (node.getAttribute("class") ?? "")
		.split(/\s+/)
		.filter((name) => SAFE_CLASS.test(name));
	if (kept.length > 0) {
		node.setAttribute("class", kept.join(" "));
	} else {
		node.removeAttribute("class");
	}
});

export function sanitizeHtml(html: string): string {
	return DOMPurify.sanitize(html, sanitizeConfig);
}

export function renderMarkdown(text: string): string {
	const html = markdown.parse(text, { async: false });
	return sanitizeHtml(html);
}
