<script lang="ts">
interface CompletionItem {
	readonly id: string;
	readonly primary: string;
	readonly secondary: string;
}

interface Props {
	readonly id: string;
	readonly label: string;
	readonly items: readonly CompletionItem[];
	readonly active: number;
}

let { id, label, items, active }: Props = $props();
</script>

<ul {id} class="completion" role="listbox" aria-label={label}>
	{#each items as item, index (item.id)}
		<li
			id="{id}-{index}"
			role="option"
			aria-selected={index === active ? "true" : "false"}
			class:active={index === active}
		>
			<span class="primary">{item.primary}</span>
			<span class="secondary">{item.secondary}</span>
		</li>
	{/each}
</ul>

<style>
	.completion {
		display: grid;
		margin: var(--space-2) 0 0;
		padding: 0;
		max-height: 16rem;
		overflow-y: auto;
		list-style: none;
		border: var(--border-width) solid var(--line);
		border-radius: var(--radius-card);
		background: var(--surface);
	}

	li {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2) var(--space-3);
		padding: var(--space-2) var(--space-3);
		font-size: var(--size-small);
	}

	li.active {
		background: var(--tone);
		box-shadow: inset 2px 0 0 var(--text);
	}

	.primary {
		font-family: var(--face-mono);
		font-weight: var(--weight-strong);
	}

	.secondary {
		color: var(--dim);
		overflow-wrap: anywhere;
	}
</style>
