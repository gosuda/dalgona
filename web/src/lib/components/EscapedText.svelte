<script lang="ts">
import { type DisplayReveal, displaySegments } from "#lib/inputtext.ts";

interface Props {
	readonly text: string;
	readonly reveal?: DisplayReveal;
}

let { text, reveal = "controls" }: Props = $props();
const segments = $derived(displaySegments(text, reveal));
const plain = $derived(segments.every((segment) => segment.escape === null));
</script>

{#if plain}{text}{:else}<bdi>{#each segments as segment}{#if segment.escape === null}{segment.text}{:else}<span class="escape" data-escape={segment.escape} title={segment.escape}>{segment.text}</span>{/if}{/each}</bdi>{/if}

<style>
	.escape {
		font-size: 0;
		unicode-bidi: isolate;
	}

	.escape::before {
		content: attr(data-escape);
		border: var(--border-width) solid var(--line);
		border-radius: var(--radius-tag);
		padding-inline: var(--space-1);
		color: var(--dim);
		font-family: var(--face-mono);
		font-size: var(--size-caption);
	}
</style>
