<script lang="ts">
import type { TreeTurn } from "#lib/types.ts";

interface Props {
	readonly name: string;
	readonly turns: readonly TreeTurn[];
	readonly selected: string | null;
	readonly emptyText: string;
	readonly onSelect: (turnId: string) => void;
}

let { name, turns, selected, emptyText, onSelect }: Props = $props();
</script>

{#if turns.length === 0}
	<p class="empty">{emptyText}</p>
{:else}
	<ul class="turns">
		{#each turns as turn (turn.turnId)}
			<li>
				<label>
					<input
						type="radio"
						{name}
						value={turn.turnId}
						checked={turn.turnId === selected}
						onchange={() => onSelect(turn.turnId)}
					/>
					<span class="preview">{turn.preview === "" ? "Untitled turn" : turn.preview}</span>
					{#if turn.active}
						<span class="active">Active</span>
					{/if}
				</label>
			</li>
		{/each}
	</ul>
{/if}

<style>
	.turns {
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.turns li + li {
		margin-block-start: var(--space-2);
	}

	.turns label {
		display: flex;
		align-items: baseline;
		gap: var(--space-3);
		min-width: 0;
		cursor: pointer;
	}

	.preview {
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.active {
		color: var(--dim);
		font-size: var(--size-caption);
	}

	.empty {
		color: var(--dim);
	}
</style>
