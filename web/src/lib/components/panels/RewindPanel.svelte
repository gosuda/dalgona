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
const active = $derived(turns.filter((turn) => turn.active));

$effect(() => {
	if (open) void load();
});

async function load(): Promise<void> {
	error = "";
	try {
		turns = await session.threadTurns();
		const preset = session.panelTurnId;
		selected = active.some((turn) => turn.turnId === preset) ? preset : null;
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	}
}

async function rewind(): Promise<void> {
	if (selected === null) return;
	error = "";
	try {
		await session.rewindTo(selected);
		session.closePanel();
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	}
}
</script>

<Panel title="Rewind to a turn" {open} onClose={() => session.closePanel()}>
	{#snippet children()}
		<TurnList
			name="rewind-turn"
			turns={active}
			{selected}
			emptyText="No turn on the active path can be rewound."
			onSelect={(turnId) => (selected = turnId)}
		/>
		<p class="confirm">
			Rewind to before this turn? Later turns leave the active path. Files changed by later turns are not reverted.
		</p>
		{#if error !== ""}
			<p class="error" role="alert">{error}</p>
		{/if}
	{/snippet}
	{#snippet footer()}
		<button type="button" class="point" disabled={selected === null} onclick={() => void rewind()}>Rewind</button>
		<button type="button" class="quiet" onclick={() => session.closePanel()}>Keep</button>
	{/snippet}
</Panel>

<style>
	.confirm {
		margin-block: var(--space-4) 0;
		color: var(--dim);
	}

	.error {
		color: var(--failed-text);
		font-weight: var(--weight-strong);
	}
</style>
