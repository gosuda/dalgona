<script lang="ts">
import {
	findKeywords,
	type KeywordMatch,
	keywordGradient,
} from "../keywords.js";
import type { KeywordDefinition } from "../types.js";

interface Props {
	readonly text: string;
	readonly keywords: readonly KeywordDefinition[];
	readonly active?: boolean;
}

interface Piece {
	readonly key: string;
	readonly value: string;
	readonly match: KeywordMatch | undefined;
}

let { text, keywords, active = false }: Props = $props();

function pieces(
	source: string,
	table: readonly KeywordDefinition[],
): readonly Piece[] {
	const points = Array.from(source);
	const hits = findKeywords(source, table);
	const out: Piece[] = [];
	let cursor = 0;
	for (const hit of hits) {
		if (hit.start > cursor) {
			out.push({
				key: `t${cursor}`,
				value: points.slice(cursor, hit.start).join(""),
				match: undefined,
			});
		}
		out.push({
			key: `k${hit.start}`,
			value: points.slice(hit.start, hit.end).join(""),
			match: hit,
		});
		cursor = hit.end;
	}
	if (cursor < points.length) {
		out.push({
			key: `t${cursor}`,
			value: points.slice(cursor).join(""),
			match: undefined,
		});
	}
	return out;
}

const parts = $derived(pieces(text, keywords));
</script>

{#each parts as part (part.key)}
	{#if part.match}
		<span class="keyword-surface">
			<span
				class="keyword"
				class:active={active}
				style:background-image={keywordGradient(part.match.hue)}
			>{part.value}</span>
		</span>
	{:else}
		{part.value}
	{/if}
{/each}

<style>
	.keyword-surface {
		display: inline-block;
		border-radius: var(--radius-small);
		background: var(--color-keyword-surface);
		padding: 0 var(--space-1);
	}

	.keyword {
		background-clip: text;
		-webkit-background-clip: text;
		-webkit-text-fill-color: transparent;
		color: transparent;
		background-size: 200% 100%;
		background-position: 0% 50%;
	}

	@media (prefers-reduced-motion: no-preference) {
		.keyword.active {
			animation: keyword-glow 1200ms ease-out 1;
		}
	}

	@media (forced-colors: active) {
		.keyword-surface {
			border: 1px solid CanvasText;
			background: Canvas;
		}

		.keyword {
			background-image: none;
			color: CanvasText;
			-webkit-text-fill-color: CanvasText;
		}
	}

	@keyframes keyword-glow {
		from {
			background-position: 0% 50%;
		}
		to {
			background-position: 100% 50%;
		}
	}
</style>
