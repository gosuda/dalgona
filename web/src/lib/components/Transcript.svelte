<script lang="ts">
import { type Component, untrack } from "svelte";
import Mark from "#lib/components/Mark.svelte";
import ReviewView from "#lib/components/ReviewView.svelte";
import AdvisorNote from "#lib/components/rows/AdvisorNote.svelte";
import CommandCard from "#lib/components/rows/CommandCard.svelte";
import FileChangeRow from "#lib/components/rows/FileChangeRow.svelte";
import MessageRow from "#lib/components/rows/MessageRow.svelte";
import PlanPanel from "#lib/components/rows/PlanPanel.svelte";
import SubagentBlock from "#lib/components/rows/SubagentBlock.svelte";
import TextRow from "#lib/components/rows/TextRow.svelte";
import ThinkingRow from "#lib/components/rows/ThinkingRow.svelte";
import ToolCard from "#lib/components/rows/ToolCard.svelte";
import type { RowProps } from "#lib/components/rows/types.ts";
import { handoffFocus } from "#lib/focus.ts";
import {
	type ScrollSnapshot,
	shouldAutoScrollForUpdate,
} from "#lib/reducer.ts";
import type {
	EditReview,
	OutputPage,
	ThreadSummary,
	TranscriptItem,
	TurnPlan,
	TurnStatus,
} from "#lib/types.ts";
import {
	groupTools,
	isFinishedEntry,
	isLiveItem,
	isToolGroup,
	presentRow,
	type TranscriptEntry,
	toolGroupSummary,
	type Verbosity,
} from "#lib/verbosity.ts";

interface Props {
	threadId: string | null;
	items: readonly TranscriptItem[];
	turnStatus: TurnStatus | null;
	error: string | null;
	plan: TurnPlan | null;
	turnReview: EditReview | null;
	connected: boolean;
	loading: boolean;
	awaitingApproval: ReadonlySet<string>;
	childThreadIds: readonly string[];
	threads: readonly ThreadSummary[];
	userAgent: string | null;
	starting: boolean;
	onOpenThread: (threadId: string) => void;
	onStart: () => void;
	verbosity?: Verbosity;
	readOutput: (itemId: string, offset: number) => Promise<OutputPage>;
	onRewind?: (turnId: string) => void;
}

const rowComponents: Record<TranscriptItem["kind"], Component<RowProps>> = {
	userMessage: MessageRow,
	agentMessage: MessageRow,
	reasoning: ThinkingRow,
	commandExecution: CommandCard,
	fileChange: FileChangeRow,
	toolCall: ToolCard,
	plan: TextRow,
	advisorNote: AdvisorNote,
	contextCompaction: TextRow,
	unsupported: TextRow,
};

let {
	threadId,
	items,
	turnStatus,
	error,
	plan,
	turnReview,
	connected,
	loading,
	awaitingApproval,
	childThreadIds,
	threads,
	userAgent,
	starting,
	onOpenThread,
	onStart,
	verbosity = "medium",
	readOutput,
	onRewind,
}: Props = $props();
let viewport = $state<HTMLElement>();
let running = $derived(turnStatus === "inProgress");
const entries = $derived(groupTools(items, verbosity));
let reviewTarget = $state<TranscriptEntry | null>(null);
let followedThread: string | null = null;
let following = $state(true);
let unseen = $state(0);
const showJump = $derived(!following);
let scrollQueued = false;
let scrolledSnapshot: ScrollSnapshot = { count: 0, lastId: null };

function scroller(): Element | null {
	for (
		let element = viewport;
		element;
		element = element.parentElement ?? undefined
	) {
		const overflow = getComputedStyle(element).overflowY;
		if (
			(overflow === "auto" || overflow === "scroll") &&
			element.scrollHeight > element.clientHeight
		) {
			return element;
		}
	}
	return document.scrollingElement;
}

$effect(() => {
	document.addEventListener("scroll", trackFollowing, true);
	return () => document.removeEventListener("scroll", trackFollowing, true);
});

$effect(() => {
	const currentItems = items;
	const switched = threadId !== followedThread;
	if (switched) {
		followedThread = threadId;
		following = true;
		unseen = 0;
		scrolledSnapshot = { count: 0, lastId: null };
	}
	const nextSnapshot: ScrollSnapshot = {
		count: currentItems.length,
		lastId: currentItems.at(-1)?.id ?? null,
	};
	const previousSnapshot = scrolledSnapshot;
	scrolledSnapshot = nextSnapshot;
	const followed = untrack(() => following);
	if (!switched && !followed) {
		unseen += Math.max(0, nextSnapshot.count - previousSnapshot.count);
	}
	if (!shouldAutoScrollForUpdate(running, previousSnapshot, nextSnapshot))
		return;
	if (!followed) return;
	if (scrollQueued) return;
	scrollQueued = true;
	requestAnimationFrame(() => {
		scrollQueued = false;
		const target = scroller();
		if (target && following) {
			target.scrollTo({ top: target.scrollHeight, behavior: "auto" });
		}
	});
});

function trackFollowing(): void {
	const target = scroller();
	if (!target) return;
	const distance = target.scrollHeight - target.scrollTop - target.clientHeight;
	following = distance < 32;
	if (following) unseen = 0;
}

function entryKey(entry: TranscriptEntry): string {
	const item = isToolGroup(entry) ? entry.items[0] : entry;
	return JSON.stringify([
		isToolGroup(entry) ? "group" : "item",
		item?.turnId,
		item?.id,
	]);
}

function openReview(target: TranscriptEntry): void {
	reviewTarget = target;
}

function openLatestReview(event: KeyboardEvent): void {
	if (reviewTarget !== null) return;
	if (!event.ctrlKey || event.key.toLowerCase() !== "o") return;
	const focused = document.activeElement;
	const inTranscript =
		viewport !== undefined && focused !== null && viewport.contains(focused);
	const inComposer = focused !== null && focused.id === "prompt";
	if (!inTranscript && !inComposer) return;
	const latest = [...entries].reverse().find(isFinishedEntry);
	if (latest === undefined) return;
	event.preventDefault();
	openReview(latest);
}

function lastRow(): HTMLElement | undefined {
	const rows = viewport?.querySelectorAll<HTMLElement>(".items > li");
	const row = rows?.[rows.length - 1];
	if (row) row.tabIndex = -1;
	return row;
}

function jumpToLatest(): void {
	following = true;
	unseen = 0;
	const target = scroller();
	if (target) target.scrollTo({ top: target.scrollHeight, behavior: "auto" });
	lastRow()?.focus({ preventScroll: true });
}
</script>

<svelte:window onkeydown={openLatestReview} />

<section id="transcript" class="transcript" aria-label="Transcript" tabindex="-1" bind:this={viewport}>
	<div class="transcript-inner">
		{#if !threadId}
			<section class="empty-state" aria-labelledby="empty-title">
				<Mark />
				<p class="kicker">No thread open</p>
				<h2 id="empty-title">Pick a thread or start a new one</h2>
				<p>{connected ? "Start a thread or pick one from the list." : "Connect to dalgona to see your threads."}</p>
				<button class="primary" type="button" aria-disabled={!connected || starting ? "true" : undefined} onclick={() => { if (connected && !starting) onStart(); }}>Start thread</button>
				{#if userAgent}
					<p class="version">{userAgent}</p>
				{/if}
			</section>
		{:else if !loading && items.length === 0}
			<section class="empty-state" aria-labelledby="new-thread-title">
				<p class="kicker">Thread ready</p>
				<h2 id="new-thread-title">Start with the outcome.</h2>
				<p>Describe the task, the constraints, and what the agent should verify.</p>
			</section>
		{:else}
			<ol class="items">
				{#each entries as entry (entryKey(entry))}
					{#if isToolGroup(entry)}
						<li class="tool-group-row">
							<p class="tool-group">{toolGroupSummary(entry)}</p>
							<button type="button" aria-label="Open full text" onclick={() => openReview(entry)}>Open</button>
						</li>
					{:else}
						{@const Row = rowComponents[entry.kind]}
						{@const presentation = presentRow(entry, verbosity, isLiveItem(entry))}
						{#if presentation.mode !== "hidden"}
							<li class:from-user={entry.kind === "userMessage"}>
								<Row item={entry} {running} awaiting={awaitingApproval} {presentation} onOpen={() => openReview(entry)} {onRewind} />
							</li>
						{/if}
					{/if}
				{/each}
			</ol>
		{/if}

		{#if childThreadIds.length > 0}
			<SubagentBlock childIds={childThreadIds} {threads} onOpen={onOpenThread} />
		{/if}

		{#if turnReview !== null}
			<p class="turn-review" data-verdict={turnReview.verdict}>{turnReview.summary}</p>
		{/if}
		<PlanPanel {plan} />

		{#if running}
			<p class="turn-outcome">
				{error ? `Retrying after an error: ${error}` : "Working · new output appears here"}
			</p>
		{:else if turnStatus === "failed"}
			<p class="turn-outcome failed">
				The turn failed{error ? `: ${error}` : ""}. Send your message again to retry, or start a new thread.
			</p>
		{:else if turnStatus === "interrupted"}
			<p class="turn-outcome">Turn interrupted. Send a message to continue.</p>
		{/if}
	</div>

	{#if showJump}
		<button class="jump" type="button" onclick={jumpToLatest} {@attach handoffFocus(lastRow)}>Jump to latest{unseen > 0 ? ` · ${unseen} new` : ""}</button>
	{/if}
		<ReviewView target={reviewTarget} {readOutput} onClose={() => (reviewTarget = null)} />
</section>

<style>
	.transcript {
		min-width: 0;
		min-height: 0;
		overflow-y: auto;
		overscroll-behavior: contain;
		scroll-padding-block-start: var(--space-6);
	}

	.transcript-inner {
		width: min(100%, 56rem);
		margin-inline: auto;
		padding: var(--space-8) var(--space-6);
	}

	.items {
		display: grid;
		gap: var(--space-6);
		list-style: none;
		margin: 0;
		padding: 0;
	}

	.items > li {
		min-width: 0;
	}

	.from-user {
		padding-inline-start: var(--space-8);
	}

	.turn-outcome {
		max-width: 70ch;
		margin: var(--space-6) 0 0;
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 600;
		overflow-wrap: break-word;
	}

	.turn-outcome.failed {
		border-inline-start: 2px solid var(--failed-text);
		background: var(--surface);
		color: var(--text);
		padding: var(--space-2) var(--space-3);
	}

	.empty-state {
		max-width: 38rem;
		padding-block-start: var(--space-8);
	}

	.kicker {
		margin: 0 0 var(--space-3);
		color: var(--accent-text);
		font-size: var(--size-caption);
		font-weight: 650;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	.empty-state h2 {
		margin: 0;
		font-size: var(--size-title);
		font-weight: 600;
		letter-spacing: -0.02em;
		line-height: 1.15;
		text-wrap: balance;
	}

	.empty-state p:last-child {
		max-width: 55ch;
		margin: var(--space-4) 0 0;
		color: var(--dim);
		line-height: 1.6;
		text-wrap: pretty;
	}

	.version {
		color: var(--dim);
		font-size: var(--size-caption);
	}

	@media (max-width: 40rem) {
		.transcript-inner {
			padding: var(--space-6) var(--space-4);
		}

		.from-user {
			padding-inline-start: 0;
		}
	}

	.turn-review {
		margin: var(--space-4) 0 0;
		color: var(--dim);
		font-size: var(--size-small);
	}

	.turn-review[data-verdict="flagged"] {
		color: var(--warning-text);
	}

	.turn-review[data-verdict="error"] {
		color: var(--failed-text);
		font-weight: var(--weight-strong);
	}
	.tool-group {
		margin: 0;
		color: var(--dim);
		font-size: var(--size-small);
	}

	.jump {
		position: sticky;
		inset-block-end: var(--space-3);
		display: block;
		width: fit-content;
		margin: var(--space-3) auto 0;
	}

	@media (max-width: 52rem) {
		.transcript {
			overflow-y: visible;
			overscroll-behavior: auto;
		}

		.jump {
			position: fixed;
			inset-inline: 0;
			inset-block-end: calc(var(--space-3) + var(--dock-height, 6rem));
			margin-inline: auto;
		}
	}
</style>
