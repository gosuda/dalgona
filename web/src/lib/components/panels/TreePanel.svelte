<script lang="ts">
import Panel from "#lib/components/panels/Panel.svelte";
import TurnList from "#lib/components/panels/TurnList.svelte";
import type { Session } from "#lib/session.svelte.ts";
import type { TreeTurn } from "#lib/types.ts";

interface Props {
	readonly session: Session;
	readonly open: boolean;
}

let { session, open }: Props = $props();
let turns = $state<readonly TreeTurn[]>([]);
let selected = $state<string | null>(null);
let error = $state("");

$effect(() => {
	if (open) void load();
});

async function load(): Promise<void> {
	error = "";
	try {
		turns = await session.threadTurns();
		selected = turns.find((turn) => turn.active)?.turnId ?? null;
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	}
}

async function move(): Promise<void> {
	if (selected === null) return;
	error = "";
	try {
		await session.setLeaf(selected);
		session.closePanel();
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	}
}
</script>

<Panel title="Move to another branch" {open} onClose={() => session.closePanel()}>
	{#snippet children()}
		<p>Choose the turn that the thread continues from.</p>
		<TurnList
			name="tree-turn"
			{turns}
			{selected}
			emptyText="This thread has no branches yet."
			onSelect={(turnId) => (selected = turnId)}
		/>
		{#if error !== ""}
			<p class="error" role="alert">{error}</p>
		{/if}
	{/snippet}
	{#snippet footer()}
		<button type="button" class="point" disabled={selected === null} onclick={() => void move()}>Switch branch</button>
		<button type="button" class="quiet" onclick={() => session.closePanel()}>Cancel</button>
	{/snippet}
</Panel>

<style>
	.error {
		color: var(--failed-text);
		font-weight: var(--weight-strong);
	}
</style>
