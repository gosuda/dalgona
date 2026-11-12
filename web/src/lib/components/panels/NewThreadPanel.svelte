<script lang="ts">
import Panel from "#lib/components/panels/Panel.svelte";
import type { Session } from "#lib/session.svelte.ts";

interface Props {
	readonly session: Session;
	readonly open: boolean;
}

let { session, open }: Props = $props();

function start(): void {
	void session.createThread();
	session.closePanel();
}
</script>

<Panel title="Start a new thread?" {open} onClose={() => session.closePanel()}>
	{#snippet children()}
		<p>The new thread uses the selected model.</p>
	{/snippet}
	{#snippet footer()}
		<button type="button" class="point" onclick={start}>Start thread</button>
		<button type="button" class="quiet" onclick={() => session.closePanel()}>Cancel</button>
	{/snippet}
</Panel>
