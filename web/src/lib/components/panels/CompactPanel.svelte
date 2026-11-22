<script lang="ts">
import Panel from "#lib/components/panels/Panel.svelte";
import type { Session } from "#lib/session.svelte.ts";

interface Props {
	readonly session: Session;
	readonly open: boolean;
}

let { session, open }: Props = $props();
let error = $state("");
let busy = $state(false);

async function compact(): Promise<void> {
	if (busy) return;
	busy = true;
	error = "";
	try {
		await session.compactThread();
		session.closePanel();
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	} finally {
		busy = false;
	}
}
</script>

<Panel title="Compact the context" {open} onClose={() => session.closePanel()}>
	{#snippet children()}
		<p>Compact this thread? Earlier turns stay in the journal.</p>
		{#if error !== ""}
			<p class="error" role="alert">{error}</p>
		{/if}
	{/snippet}
	{#snippet footer()}
		<button type="button" class="point" aria-disabled={busy ? "true" : undefined} onclick={() => void compact()}>Compact</button>
		<button type="button" class="quiet" onclick={() => session.closePanel()}>Cancel</button>
	{/snippet}
</Panel>

<style>
	.error {
		color: var(--failed-text);
		font-weight: var(--weight-strong);
	}
</style>
