import { describe, expect, test } from "vitest";
import { renderMarkdown, sanitizeHtml } from "#lib/markdown.ts";

function parse(source: string): HTMLElement {
	const root = document.createElement("div");
	root.innerHTML = renderMarkdown(source);
	return root;
}

function assertNoActiveContent(root: HTMLElement): void {
	expect(root.querySelector("script, img, svg, iframe")).toBeNull();
	for (const element of Array.from(root.querySelectorAll("*"))) {
		for (const attribute of Array.from(element.attributes)) {
			expect(attribute.name.startsWith("on")).toBe(false);
		}
	}
	for (const link of Array.from(root.querySelectorAll("a[href]"))) {
		expect(link.getAttribute("href")).toMatch(/^(https?:|mailto:)/);
	}
}

describe("sanitising rule", () => {
	test("reads element names through Node.prototype like a browser", () => {
		const element = document.createElement("pre");
		const getter = Object.getOwnPropertyDescriptor(
			Node.prototype,
			"nodeName",
		)?.get;
		expect(getter?.call(element)).toBe("PRE");
	});

	test.each([
		["ok <script>alert(1)</script>", "ok"],
		["ok <img src=x onerror=alert(1)>", "ok"],
		["[x](javascript:alert(1))", "x"],
		["ok <svg onload=alert(1)>", "ok"],
		['<a href="data:text/html,x">x</a>', "x"],
		["ok <iframe src=//x>", "ok"],
		["![a](http://x/y.png)", "image: a"],
	])("drops active content from %s and keeps %s", (source, safe) => {
		const root = parse(source);
		assertNoActiveContent(root);
		expect(root.textContent).toContain(safe);
	});

	test("renders fenced go code with highlight classes", () => {
		const root = parse("```go\nfunc main() {}\n```\n");
		const code = root.querySelector("pre > code.hljs");
		expect(code).not.toBeNull();
		expect(code?.querySelector("span.hljs-keyword")).not.toBeNull();
		expect(code?.textContent).toContain("func main() {}");
	});

	test("shifts headings down two levels", () => {
		const root = parse("# T\n");
		expect(root.querySelector("h3")?.textContent).toBe("T");
		expect(root.querySelector("h1")).toBeNull();
	});

	test("gives each safe link a rel and a target", () => {
		const root = parse("[site](https://example.com)\n");
		const link = root.querySelector("a");
		expect(link?.getAttribute("href")).toBe("https://example.com");
		expect(link?.getAttribute("rel")).toBe("noopener noreferrer nofollow");
		expect(link?.getAttribute("target")).toBe("_blank");
		expect(link?.textContent).toBe("site");
	});

	test("shows raw html as escaped text", () => {
		const root = parse("<b>bold</b>\n");
		expect(root.querySelector("b")).toBeNull();
		expect(root.textContent).toContain("<b>bold</b>");
	});
});

describe("sanitiser seam", () => {
	function sanitized(html: string): HTMLElement {
		const root = document.createElement("div");
		root.innerHTML = sanitizeHtml(html);
		return root;
	}

	test.each([
		["<p>ok</p><script>alert(1)</script>", "script"],
		['<p>ok <img src="x" onerror="alert(1)"></p>', "img"],
		['<p>ok <svg onload="alert(1)"></svg></p>', "svg"],
		['<p>ok <iframe src="//x"></iframe></p>', "iframe"],
	])("removes the element of %s and keeps its text", (html, element) => {
		const root = sanitized(html);
		expect(root.querySelector(element)).toBeNull();
		expect(root.textContent).toContain("ok");
	});

	test.each([
		['<p><a href="javascript:alert(1)">ok</a></p>'],
		['<p><a href="data:text/html,x">ok</a></p>'],
	])("drops the unsafe href of %s and keeps the link text", (html) => {
		const link = sanitized(html).querySelector("a");
		expect(link).not.toBeNull();
		expect(link?.hasAttribute("href")).toBe(false);
		expect(link?.textContent).toBe("ok");
	});

	test.each([
		['<span class="hljs-keyword evil">if</span>', "hljs-keyword"],
		['<code class="hljs language-go evil">x</code>', "hljs language-go"],
		['<span class="evil">y</span>', null],
	])("keeps only the highlight classes of %s", (html, kept) => {
		const element = sanitized(html).firstElementChild;
		expect(element?.getAttribute("class")).toBe(kept);
	});
});
