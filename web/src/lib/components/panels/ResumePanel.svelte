<script lang="ts">
import Panel from "#lib/components/panels/Panel.svelte";
import { handoffFocus } from "#lib/focus.ts";
import {
	faultNotices,
	refuseSingleLineInsertion,
	searchEligible,
	selectDeletedCluster,
} from "#lib/inputtext.ts";
import { threadTitle } from "#lib/labels.ts";
import { appendThreadPage } from "#lib/reducer.ts";
import type { Session } from "#lib/session.svelte.ts";
import type { ThreadSummary } from "#lib/types.ts";
import type { ThreadSearchHit } from "#lib/validation.ts";

interface Props {
	readonly session: Session;
	readonly open: boolean;
}

let { session, open }: Props = $props();
let threads = $state<readonly ThreadSummary[]>([]);
let nextCursor = $state<string | null>(null);
let loading = $state(false);
let listRequest = 0;
let filter = $state("");
let filterField: HTMLInputElement;
let filterError = $state("");
let error = $state("");
let hits = $state<readonly ThreadSearchHit[]>([]);
let searchError = $state("");
let searching = $state(false);
let retry = $state(0);
let opening = $state(false);
let attempt = 0;
const canLoadPage = $derived(
	!loading && (error !== "" || session.connection === "connected"),
);
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
	if (open) return;
	opening = false;
});

$effect(() => {
	if (!open || session.connection === "connecting") return;
	void load(null);
	return () => {
		listRequest += 1;
	};
});

$effect(() => {
	const query = filter;
	void retry;
	hits = [];
	searchError = "";
	searching = false;
	if (!open || !searchEligible(query)) return;
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
async function load(cursor: string | null): Promise<void> {
	if (cursor !== null && loading) return;
	const request = ++listRequest;
	loading = true;
	if (cursor === null) nextCursor = null;
	try {
		const page =
			cursor === null
				? await session.listThreads()
				: await session.listOlderThreads(cursor, threads);
		if (request !== listRequest) return;
		error = "";
		threads =
			cursor === null ? page.threads : appendThreadPage(threads, page.threads);
		nextCursor = page.nextCursor;
	} catch (failure) {
		if (request === listRequest) {
			error = failure instanceof Error ? failure.message : String(failure);
		}
	} finally {
		if (request === listRequest) loading = false;
	}
}

async function choose(threadId: string): Promise<void> {
	if (opening) return;
	const opened = session.openedPanel;
	const mine = ++attempt;
	opening = true;
	try {
		await session.selectThread(threadId);
	} finally {
		if (mine === attempt) opening = false;
	}
	session.closePanelIf(opened);
}

function guardInsertion(event: ClipboardEvent | DragEvent): void {
	const fault = refuseSingleLineInsertion(event);
	filterError = fault === null ? "" : faultNotices[fault];
}
</script>

<Panel title="Open an earlier thread" {open} onClose={() => session.closePanel()}>
	{#snippet children()}
		<input
			type="search"
			bind:this={filterField}
			bind:value={filter}
			placeholder="Filter threads"
			aria-label="Filter threads"
			aria-invalid={filterError === "" ? "false" : "true"}
			aria-describedby={filterError === "" ? undefined : "resume-filter-error"}
			onpaste={guardInsertion}
			ondrop={guardInsertion}
			oninput={() => (filterError = "")}
			onkeydown={(event) => selectDeletedCluster(event, event.currentTarget)}
		/>
		{#if filterError !== ""}
			<p class="error" id="resume-filter-error" role="alert">{filterError}</p>
		{/if}
		{#if opening}
			<p role="status">Opening thread…</p>
		{/if}
		{#if error !== ""}
			<p class="error" role="alert">Unable to list threads: {error}</p>
		{/if}
		{#if shown.length === 0 && !loading && error === ""}
			<p class="empty">{threads.length === 0 ? "No threads yet." : "No threads match this filter."}</p>
		{:else if shown.length > 0}
			<ul class="threads">
				{#each shown as thread (thread.id)}
					<li>
						<button type="button" class="thread-choice" aria-disabled={opening ? "true" : undefined} onclick={() => void choose(thread.id)}>
							<span class="title">{threadTitle(thread)}</span>
						</button>
					</li>
				{/each}
			</ul>
		{/if}
		{#if nextCursor !== null || error !== ""}
			<button
				type="button" class="quiet"
				aria-disabled={canLoadPage ? undefined : "true"}
				onclick={() => { if (canLoadPage) void load(nextCursor); }}
				{@attach handoffFocus(() => filterField)}
			>{error === "" ? "Load more threads" : "Try again"}</button>
		{/if}
		{#if searchEligible(filter)}
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
							<li><button type="button" class="thread-choice" aria-disabled={opening ? "true" : undefined} onclick={() => void choose(hit.threadId)}>
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
