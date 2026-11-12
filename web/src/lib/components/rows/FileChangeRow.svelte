<script lang="ts">
import DiffCard from "#lib/components/DiffCard.svelte";
import type { RowProps } from "#lib/components/rows/types.ts";
import StateTag from "#lib/components/StateTag.svelte";
import { stateKey } from "#lib/labels.ts";
import { itemDisplayStatus } from "#lib/reducer.ts";

let { item, awaiting }: RowProps = $props();
</script>

{#if item.kind === "fileChange"}
	{@const state = stateKey(itemDisplayStatus(item.status, item.id, awaiting))}
	<article class="tool-item">
		<header>
			<span class="item-label">File changes</span>
			{#if state}
				<StateTag state={state} />
			{:else}
				<span class="item-status">{item.status}</span>
			{/if}
		</header>
		{#each item.changes as change, index (`${index}:${change.path}`)}
			<DiffCard {change} open={false} />
		{/each}
	</article>
{/if}

<style>
	.tool-item {
		min-width: 0;
		border: 1px solid var(--line);
		border-radius: var(--radius-tag);
		background: var(--surface);
		padding: var(--space-4);
	}

	.tool-item header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.item-label {
		display: block;
		margin-block-end: var(--space-2);
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 650;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	.item-status {
		color: var(--dim);
		font-size: var(--size-caption);
		font-variant-numeric: tabular-nums;
	}
</style>
