<script lang="ts">
interface Props {
	items: readonly string[];
	onEdit: (index: number) => void;
	onRemove: (index: number) => void;
}

let { items, onEdit, onRemove }: Props = $props();

function firstLine(text: string): string {
	return text.split("\n", 1)[0] ?? "";
}
</script>

{#if items.length > 0}
	<ol class="queue" aria-label="Queued follow-ups">
		{#each items as item, index (index)}
			<li>
				<span class="text">{firstLine(item)}</span>
				<button class="quiet" type="button" onclick={() => onEdit(index)}>Edit</button>
				<button class="quiet" type="button" onclick={() => onRemove(index)}>Remove</button>
			</li>
		{/each}
	</ol>
{/if}

<style>
	.queue {
		display: grid;
		gap: var(--space-2);
		margin: 0 0 var(--space-3);
		padding: 0;
		list-style: none;
		font-size: var(--size-small);
	}

	li {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--space-2);
		min-width: 0;
	}

	.text {
		flex: 1 1 12rem;
		min-width: 0;
		overflow-wrap: anywhere;
	}
</style>
