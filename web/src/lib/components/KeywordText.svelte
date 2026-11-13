<script lang="ts">
import type { KeywordMatch } from "#lib/types.ts";

interface Props {
	readonly text: string;
	readonly matches: readonly KeywordMatch[];
}

interface Piece {
	readonly key: string;
	readonly value: string;
	readonly marked: boolean;
}

let { text, matches }: Props = $props();

function pieces(
	source: string,
	found: readonly KeywordMatch[],
): readonly Piece[] {
	const bytes = new TextEncoder().encode(source);
	const decoder = new TextDecoder();
	const out: Piece[] = [];
	let cursor = 0;
	for (const match of found) {
		const { start, end } = match.byteRange;
		if (start < cursor || end <= start || end > bytes.length) continue;
		if (start > cursor) {
			out.push({
				key: `t${cursor}`,
				value: decoder.decode(bytes.slice(cursor, start)),
				marked: false,
			});
		}
		out.push({
			key: `k${start}`,
			value: decoder.decode(bytes.slice(start, end)),
			marked: true,
		});
		cursor = end;
	}
	if (cursor < bytes.length) {
		out.push({
			key: `t${cursor}`,
			value: decoder.decode(bytes.slice(cursor)),
			marked: false,
		});
	}
	return out;
}

const parts = $derived(pieces(text, matches));
</script>

{#each parts as part (part.key)}
	{#if part.marked}
		<mark class="keyword">{part.value}</mark>
	{:else}
		{part.value}
	{/if}
{/each}

<style>
	.keyword {
		background: none;
		color: inherit;
		font-weight: 650;
		text-decoration: underline;
		text-decoration-thickness: 2px;
		text-underline-offset: 0.2em;
	}
</style>
