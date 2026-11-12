<script lang="ts">
import StateTag from "#lib/components/StateTag.svelte";
import { stateKey, threadTitle } from "#lib/labels.ts";
import type { ThreadSummary } from "#lib/types.ts";

interface Props {
	readonly childIds: readonly string[];
	readonly threads: readonly ThreadSummary[];
	readonly onOpen: (threadId: string) => void;
}

let { childIds, threads, onOpen }: Props = $props();
</script>

<section class="subagents" aria-label="Subagents">
	<span class="item-label">Subagents</span>
	<ul>
		{#each childIds as childId (childId)}
			{@const child = threads.find((thread) => thread.id === childId)}
			<li>
				<span class="child-title">{child ? threadTitle(child) : childId}</span>
				<StateTag state={stateKey(child?.status ?? "unknown", child?.activeFlags ?? []) ?? "idle"} />
				<button type="button" onclick={() => onOpen(childId)}>Open</button>
			</li>
		{/each}
	</ul>
</section>

<style>
	.subagents {
		max-width: 70ch;
		margin-block-start: var(--space-6);
		font-size: var(--size-small);
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

	.subagents ul {
		display: grid;
		gap: var(--space-2);
		list-style: none;
		margin: 0;
		padding: 0;
	}

	.subagents li {
		display: flex;
		align-items: baseline;
		flex-wrap: wrap;
		gap: var(--space-2) var(--space-3);
	}

	.child-title {
		min-width: 0;
		overflow-wrap: anywhere;
	}
</style>
