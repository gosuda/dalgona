import DOMPurify from "dompurify";

export const DIAGRAM_LANGS: Set<string> = new Set([
	"mermaid",
	"d2",
	"nomnoml",
	"svg",
]);

export function isDiagramLang(lang: string): boolean {
	return DIAGRAM_LANGS.has(lang);
}

const sanitizeConfig = {
	USE_PROFILES: { svg: true, svgFilters: true, html: true },
};

export function sanitizeSvg(svg: string): string {
	return DOMPurify.sanitize(svg, sanitizeConfig);
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
