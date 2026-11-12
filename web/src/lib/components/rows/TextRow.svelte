<script lang="ts">
import type { RowProps } from "#lib/components/rows/types.ts";
import { kindLabel } from "#lib/labels.ts";

let { item }: RowProps = $props();
</script>

{#if item.kind === "plan"}
	<article class="plan">
		<span class="item-label">Plan</span>
		<p>{item.text}</p>
	</article>
{:else if item.kind === "contextCompaction"}
	<p class="compaction">Context compacted · earlier turns remain in the journal</p>
{:else if item.kind === "unsupported"}
	<p class="compaction">{kindLabel(item.label)}</p>
{/if}

<style>
	.item-label {
		display: block;
		margin-block-end: var(--space-2);
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 650;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	.plan {
		border-block: 1px solid var(--line);
		padding-block: var(--space-3);
	}

	.plan p {
		max-width: 70ch;
		margin: 0;
		line-height: 1.6;
		overflow-wrap: break-word;
		white-space: pre-wrap;
		text-wrap: pretty;
	}

	.compaction {
		margin: 0;
		color: var(--dim);
		font-size: var(--size-caption);
		text-align: center;
	}
</style>
