<script lang="ts">
import { renderDiagram } from "#lib/diagrams.ts";
import { renderMarkdown } from "#lib/markdown.ts";

interface Props {
	readonly text: string;
	readonly streaming: boolean;
}

let { text, streaming }: Props = $props();
let frameHtml = $state<string | null>(null);
let latest = "";
let frame = 0;
let host: HTMLDivElement | null = $state(null);
const html = $derived(
	streaming ? (frameHtml ?? renderMarkdown(text)) : renderMarkdown(text),
);

$effect(() => {
	latest = text;
	if (!streaming || frame !== 0) return;
	frame = requestAnimationFrame(() => {
		frame = 0;
		frameHtml = renderMarkdown(latest);
	});
});

$effect(() => () => {
	cancelAnimationFrame(frame);
	frame = 0;
});

$effect(() => {
	html;
	const root = host;
	if (!root) return;
	for (const block of root.querySelectorAll<HTMLElement>(
		"pre.diagram[class*=language-]",
	)) {
		const lang = block.className.match(/language-([a-z0-9+#-]+)/i)?.[1] ?? "";
		const source = block.textContent ?? "";
		renderDiagram(lang, source)
			.then((svg) => {
				const figure = document.createElement("div");
				figure.className = "diagram diagram-rendered";
				figure.innerHTML = svg;
				block.replaceWith(figure);
			})
			.catch((error: unknown) => {
				block.classList.add("diagram-error");
				block.title = `diagram render failed: ${String(error instanceof Error ? error.message : error)}`;
			});
	}
});
</script>

<div class="markdown" bind:this={host}>{@html html}</div>
