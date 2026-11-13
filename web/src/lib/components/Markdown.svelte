<script lang="ts">
import { renderMarkdown } from "#lib/markdown.ts";

interface Props {
	readonly text: string;
	readonly streaming: boolean;
}

let { text, streaming }: Props = $props();
let frameHtml = $state<string | null>(null);
let latest = "";
let frame = 0;
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
</script>

<div class="markdown">{@html html}</div>
