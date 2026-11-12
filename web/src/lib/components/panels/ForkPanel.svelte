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
	selected = null;
	error = "";
	try {
		turns = await session.threadTurns();
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	}
}

async function fork(): Promise<void> {
	if (selected === null) return;
	error = "";
	try {
		await session.forkThread(selected);
		session.closePanel();
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	}
}
</script>

<Panel title="Fork at a turn" {open} onClose={() => session.closePanel()}>
	{#snippet children()}
		<p>The fork keeps the thread up to the chosen turn and opens it.</p>
		<TurnList
			name="fork-turn"
			{turns}
			{selected}
			emptyText="This thread has no turns to fork from."
			onSelect={(turnId) => (selected = turnId)}
		/>
		{#if error !== ""}
			<p class="error" role="alert">{error}</p>
		{/if}
	{/snippet}
	{#snippet footer()}
		<button type="button" class="point" disabled={selected === null} onclick={() => void fork()}>Fork here</button>
		<button type="button" class="quiet" onclick={() => session.closePanel()}>Cancel</button>
	{/snippet}
</Panel>

<style>
	.error {
		color: var(--failed-text);
		font-weight: var(--weight-strong);
	}
</style>
