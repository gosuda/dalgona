<script lang="ts">
import Panel from "#lib/components/panels/Panel.svelte";
import { threadTitle } from "#lib/labels.ts";
import type { Session } from "#lib/session.svelte.ts";
import type { ThreadSummary } from "#lib/types.ts";

interface Props {
	readonly session: Session;
	readonly open: boolean;
}

let { session, open }: Props = $props();
let threads = $state<readonly ThreadSummary[]>([]);
let filter = $state("");
let error = $state("");
const needle = $derived(filter.trim().toLowerCase());
const shown = $derived(
	threads.filter(
		(thread) =>
			needle === "" ||
			threadTitle(thread).toLowerCase().includes(needle) ||
			thread.id.toLowerCase().includes(needle),
	),
);

$effect(() => {
	if (open) void load();
});

async function load(): Promise<void> {
	error = "";
	try {
		threads = await session.listThreads();
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	}
}

function choose(threadId: string): void {
	void session.selectThread(threadId);
	session.closePanel();
}
</script>

<Panel title="Open an earlier thread" {open} onClose={() => session.closePanel()}>
	{#snippet children()}
		<input type="search" bind:value={filter} placeholder="Filter threads" aria-label="Filter threads" />
		{#if error !== ""}
			<p class="error" role="alert">Unable to list threads: {error}</p>
			<button type="button" class="quiet" onclick={() => void load()}>Try again</button>
		{:else if shown.length === 0}
			<p class="empty">{threads.length === 0 ? "No threads yet." : "No threads match this filter."}</p>
		{:else}
			<ul class="threads">
				{#each shown as thread (thread.id)}
					<li>
						<button type="button" class="thread-choice" onclick={() => choose(thread.id)}>
							<span class="title">{threadTitle(thread)}</span>
						</button>
					</li>
				{/each}
			</ul>
		{/if}
	{/snippet}
</Panel>

<style>
	.threads {
		display: grid;
		gap: var(--space-2);
		margin: var(--space-3) 0 0;
		padding: 0;
		list-style: none;
	}

	.thread-choice {
		width: 100%;
		min-height: var(--control-height);
		text-align: start;
	}

	.title {
		overflow-wrap: anywhere;
	}

	.error {
		color: var(--failed-text);
		font-weight: var(--weight-strong);
	}

	.empty {
		color: var(--dim);
	}
</style>
