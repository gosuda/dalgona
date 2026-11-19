<script lang="ts">
import Panel from "#lib/components/panels/Panel.svelte";
import { threadTitle } from "#lib/labels.ts";
import type { Session } from "#lib/session.svelte.ts";
import type { ThreadSummary } from "#lib/types.ts";
import type { ThreadSearchHit } from "#lib/validation.ts";

interface Props {
	readonly session: Session;
	readonly open: boolean;
}

let { session, open }: Props = $props();
let threads = $state<readonly ThreadSummary[]>([]);
let filter = $state("");
let error = $state("");
let hits = $state<readonly ThreadSearchHit[]>([]);
let searchError = $state("");
let searching = $state(false);
let retry = $state(0);
const needle = $derived(filter.trim().toLowerCase());
const shown = $derived(
	threads.filter(
		(thread) =>
			needle === "" ||
			threadTitle(thread).toLowerCase().includes(needle) ||
			thread.id.toLowerCase().includes(needle) ||
			thread.preview.toLowerCase().includes(needle),
	),
);

$effect(() => {
	if (open) void load();
});

$effect(() => {
	const query = filter.trim();
	void retry;
	hits = [];
	searchError = "";
	searching = false;
	if (!open || [...query].length < 3) return;
	const controller = new AbortController();
	searching = true;
	const timer = setTimeout(() => {
		void session.searchThreads(query, controller.signal).then(
			(result) => {
				if (!controller.signal.aborted) {
					hits = result;
					searching = false;
				}
			},
			(failure: unknown) => {
				if (!controller.signal.aborted) {
					searchError =
						failure instanceof Error ? failure.message : String(failure);
					searching = false;
				}
			},
		);
	}, 300);
	return () => {
		clearTimeout(timer);
		controller.abort();
	};
});
async function load(): Promise<void> {
	error = "";
	try {
		threads = await session.listThreads();
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	}
}

async function choose(threadId: string): Promise<void> {
	await session.selectThread(threadId);
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
						<button type="button" class="thread-choice" onclick={() => void choose(thread.id)}>
							<span class="title">{threadTitle(thread)}</span>
						</button>
					</li>
				{/each}
			</ul>
		{/if}
		{#if [...filter.trim()].length >= 3}
			<section aria-label="Matches in conversations" aria-busy={searching}>
				<h3>Matches in conversations</h3>
				{#if searching}
					<p role="status">Searching conversations…</p>
				{:else if searchError !== ""}
					<p class="error" role="alert">Search failed: {searchError}.</p>
					<button type="button" class="quiet" onclick={() => retry += 1}>Try again</button>
				{:else if hits.length === 0}
					<p class="empty">No conversation mentions this text.</p>
				{:else}
					<ul class="threads">
						{#each hits as hit (hit.threadId)}
							<li><button type="button" class="thread-choice" onclick={() => void choose(hit.threadId)}>
								<span class="title">{hit.name || hit.preview || "Untitled thread"}</span>
								<span>{hit.snippet}</span>
							</button></li>
						{/each}
					</ul>
				{/if}
			</section>
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
