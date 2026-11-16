import DOMPurify from "dompurify";

const diagramLangs: Set<string> = new Set(["mermaid", "d2", "nomnoml", "svg"]);

export function isDiagramLang(lang: string): boolean {
	return diagramLangs.has(lang);
}

const forbidElements = new Set([
	"style",
	"image",
	"feimage",
	"iframe",
	"object",
	"embed",
	"audio",
	"video",
	"link",
	"base",
	"meta",
	"form",
	"input",
]);

const sanitizeConfig = {
	USE_PROFILES: { svg: true, svgFilters: true, html: true },
	FORBID_ATTR: ["style"],
};

function sanitizeSvg(svg: string): string {
	const clean = DOMPurify.sanitize(svg, sanitizeConfig);
	const doc = new DOMParser().parseFromString(clean, "image/svg+xml");
	if (doc.querySelector("parsererror")) return clean;
	for (const node of doc.querySelectorAll("*")) {
		if (forbidElements.has(node.nodeName.toLowerCase())) {
			node.remove();
		}
	}
	return new XMLSerializer().serializeToString(doc.documentElement);
}

async function renderMermaid(source: string): Promise<string> {
	const { renderMermaidSVGAsync } = await import("beautiful-mermaid");
	return renderMermaidSVGAsync(source);
}

async function renderD2(source: string): Promise<string> {
	const { D2 } = await import("@terrastruct/d2");
	const d2 = new D2();
	const compiled = await d2.compile(source);
	return d2.render(compiled.diagram);
}

async function renderNomnoml(source: string): Promise<string> {
	const nomnoml = await import("nomnoml");
	return nomnoml.renderSvg(source);
}

const renderers: Record<string, (source: string) => Promise<string>> = {
	mermaid: renderMermaid,
	d2: renderD2,
	nomnoml: renderNomnoml,
	svg: async (source) => source,
};

export async function renderDiagram(
	lang: string,
	source: string,
): Promise<string> {
	const render = renderers[lang];
	if (!render) throw new Error(`no diagram renderer for ${lang}`);
	return sanitizeSvg(await render(source));
}
