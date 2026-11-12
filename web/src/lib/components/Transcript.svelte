<script lang="ts">
import KeywordText from "#lib/components/KeywordText.svelte";
import { itemStatusLabel, kindLabel, planStatusLabel } from "#lib/labels.ts";
import {
	itemDisplayStatus,
	type ScrollSnapshot,
	shouldAutoScrollForUpdate,
} from "#lib/reducer.ts";
import type {
	JsonValue,
	KeywordDefinition,
	TranscriptItem,
	TurnPlan,
	TurnStatus,
} from "#lib/types.ts";

interface Props {
	threadId: string | null;
	items: readonly TranscriptItem[];
	turnStatus: TurnStatus | null;
	error: string | null;
	plan: TurnPlan | null;
	connected: boolean;
	keywords: readonly KeywordDefinition[];
	loading: boolean;
	awaitingApproval: ReadonlySet<string>;
}

let {
	threadId,
	items,
	turnStatus,
	error,
	plan,
	connected,
	keywords,
	loading,
	awaitingApproval,
}: Props = $props();
let viewport = $state<HTMLDivElement>();
let running = $derived(turnStatus === "inProgress");
let followedThread: string | null = null;
let following = true;
let scrollQueued = false;
let scrolledSnapshot: ScrollSnapshot = { count: 0, lastId: null };

function scroller(): Element | null {
	if (!viewport) return null;
	return viewport.scrollHeight > viewport.clientHeight
		? viewport
		: document.scrollingElement;
}

$effect(() => {
	const currentItems = items;
	if (threadId !== followedThread) {
		followedThread = threadId;
		following = true;
		scrolledSnapshot = { count: 0, lastId: null };
	}
	const nextSnapshot: ScrollSnapshot = {
		count: currentItems.length,
		lastId: currentItems.at(-1)?.id ?? null,
	};
	const previousSnapshot = scrolledSnapshot;
	scrolledSnapshot = nextSnapshot;
	if (!shouldAutoScrollForUpdate(running, previousSnapshot, nextSnapshot))
		return;
	if (!following) return;
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
}

function argumentsText(value: JsonValue): string {
	return JSON.stringify(value, null, 2) ?? "";
}
</script>

<svelte:window onscroll={trackFollowing} />

<div class="transcript" bind:this={viewport} role="log" aria-label="Thread transcript" onscroll={trackFollowing}>
	<div class="transcript-inner">
		<p class="sr-only" role="status">{loading ? "Loading transcript…" : ""}</p>

		{#if !threadId}
			<section class="empty-state" aria-labelledby="empty-title">
				<p class="kicker">No thread open</p>
				<h2 id="empty-title">Pick a thread or start a new one</h2>
				{#if connected}
					<p>Earlier threads are in the list. Start a thread to give the agent a task.</p>
				{:else}
					<p>Connect to dalgona to see your threads.</p>
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
				{#each items as item (item.id)}
					<li class:from-user={item.kind === "userMessage"}>
						{#if item.kind === "userMessage"}
							<article class="user-message">
								<span class="item-label">You</span>
								<p><KeywordText text={item.text} {keywords} /></p>
							</article>
						{:else if item.kind === "agentMessage"}
							<article class="agent-message">
								<span class="item-label">dalgona</span>
								<p>{item.text}{#if !item.complete && running}<span class="streaming" aria-hidden="true">▋</span>{/if}</p>
							</article>
						{:else if item.kind === "reasoning"}
							<details class="reasoning" open={!item.complete && running}>
								<summary>Reasoning{#if !item.complete && running} in progress{/if}</summary>
								<p>{item.text}</p>
							</details>
						{:else if item.kind === "commandExecution"}
							{@const status = itemDisplayStatus(item.status, item.id, awaitingApproval)}
							<article class="tool-item">
								<header>
									<span class="item-label">Command</span>
									<span class="item-status" class:awaiting={status === "awaitingApproval"}>{itemStatusLabel(status)}{item.exitCode === null ? "" : ` · exit ${item.exitCode}`}</span>
								</header>
								<pre class="command">{item.command}</pre>
								{#if item.output}
									<pre class="output">{item.output}</pre>
								{/if}
							</article>
						{:else if item.kind === "fileChange"}
							{@const status = itemDisplayStatus(item.status, item.id, awaitingApproval)}
							<article class="tool-item">
								<header>
									<span class="item-label">File changes</span>
									<span class="item-status" class:awaiting={status === "awaitingApproval"}>{itemStatusLabel(status)}</span>
								</header>
								{#each item.changes as change (`${change.path}:${change.kind}`)}
									<details class="file-change">
										<summary><span class="file-path">{change.path}</span> · {change.kind}</summary>
										<pre class="output">{change.diff}</pre>
									</details>
								{/each}
							</article>
						{:else if item.kind === "toolCall"}
							<article class="tool-item">
								<header>
									<span class="item-label">{item.name}</span>
									<span class="item-status">{itemStatusLabel(item.status)}</span>
								</header>
								<details>
									<summary>Arguments</summary>
									<pre class="output">{argumentsText(item.arguments)}</pre>
								</details>
								{#if item.output}
									<pre class="output">{item.output}</pre>
								{/if}
							</article>
						{:else if item.kind === "plan"}
							<article class="plan">
								<span class="item-label">Plan</span>
								<p>{item.text}</p>
							</article>
						{:else if item.kind === "advisorNote"}
							<aside class="advisor-note" data-severity={item.severity} aria-label="Advisor note">
								<span class="advisor-label">{item.advisor === "advisor" ? "advisor" : `advisor ${item.advisor}`} · {item.severity}:</span>
								{item.note}
							</aside>
						{:else if item.kind === "contextCompaction"}
							<p class="compaction">Context compacted · earlier turns remain in the journal</p>
						{:else if item.kind === "unsupported"}
							<p class="compaction">{kindLabel(item.label)}</p>
						{/if}
					</li>
				{/each}
			</ol>
		{/if}

		{#if plan}
			<section class="turn-plan" aria-label="Plan">
				<span class="item-label">Plan</span>
				{#if plan.explanation}
					<p>{plan.explanation}</p>
				{/if}
				<ol>
					{#each plan.steps as step, index (index)}
						<li data-status={step.status} aria-current={step.status === "inProgress" ? "step" : undefined}>
							<span class="plan-status">{planStatusLabel(step.status)}</span>
							<span>{step.step}</span>
						</li>
					{/each}
				</ol>
			</section>
		{/if}

		{#if running}
			<p class="turn-outcome working">
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
</div>

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

	.item-label {
		display: block;
		margin-block-end: var(--space-2);
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 650;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	.user-message {
		border-inline-start: 2px solid var(--accent-text);
		padding: var(--space-2) var(--space-4);
		background: var(--tone);
	}

	.user-message p,
	.agent-message p,
	.plan p {
		max-width: 70ch;
		margin: 0;
		line-height: 1.6;
		overflow-wrap: break-word;
		white-space: pre-wrap;
		text-wrap: pretty;
	}

	.agent-message {
		padding-block: var(--space-2);
	}

	.streaming {
		color: var(--accent-text);
		margin-inline-start: var(--space-1);
	}

	.tool-item {
		min-width: 0;
		border: 1px solid var(--line);
		border-radius: var(--radius-tag);
		background: var(--surface);
		padding: var(--space-4);
	}

	.tool-item header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.item-status {
		color: var(--dim);
		font-size: var(--size-caption);
		font-variant-numeric: tabular-nums;
	}

	.item-status.awaiting {
		color: var(--accent-text);
		font-weight: 650;
	}

	.command {
		border-radius: var(--radius-control);
		background: var(--tone);
		padding: var(--space-3);
		font-size: var(--size-small);
		line-height: 1.5;
	}

	.output {
		max-height: 24rem;
		margin-block-start: var(--space-3);
		overflow: auto;
		overscroll-behavior: contain;
		color: var(--dim);
		font-size: var(--size-caption);
		line-height: 1.55;
	}

	.reasoning,
	.file-change,
	.tool-item details {
		max-width: 70ch;
		color: var(--dim);
		font-size: var(--size-small);
	}

	.reasoning summary,
	.file-change summary,
	.tool-item summary {
		color: var(--text);
		cursor: pointer;
		font-weight: 600;
	}

	.reasoning p {
		margin-block: var(--space-3) 0;
		white-space: pre-wrap;
	}

	.file-change {
		margin-block-start: var(--space-3);
	}

	.plan {
		border-block: 1px solid var(--line);
		padding-block: var(--space-3);
	}

	.advisor-note {
		max-width: 70ch;
		border-inline-start: 2px solid var(--strong);
		padding: var(--space-1) var(--space-3);
		color: var(--dim);
		font-size: var(--size-small);
		overflow-wrap: break-word;
		white-space: pre-wrap;
	}

	.advisor-note[data-severity="concern"] {
		border-inline-start-color: var(--accent-text);
	}

	.advisor-note[data-severity="blocker"] {
		border-inline-start-color: var(--failed-text);
	}

	.advisor-label {
		font-weight: 650;
	}

	.turn-plan {
		max-width: 70ch;
		margin-block-start: var(--space-6);
		font-size: var(--size-small);
	}

	.turn-plan p {
		margin: 0 0 var(--space-2);
		color: var(--dim);
	}

	.turn-plan ol {
		display: grid;
		gap: var(--space-1);
		list-style: none;
		margin: 0;
		padding: 0;
	}

	.turn-plan li {
		display: grid;
		grid-template-columns: 6rem minmax(0, 1fr);
		gap: var(--space-2);
		overflow-wrap: break-word;
	}

	.plan-status {
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 600;
	}

	.turn-plan li[data-status="inProgress"] {
		font-weight: 650;
	}

	.turn-plan li[data-status="completed"] > span:last-child {
		color: var(--dim);
	}

	.compaction {
		margin: 0;
		color: var(--dim);
		font-size: var(--size-caption);
		text-align: center;
	}

	.turn-outcome {
		max-width: 70ch;
		margin: var(--space-6) 0 0;
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 600;
		overflow-wrap: break-word;
	}

	.turn-outcome.working {
		color: var(--added-text);
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
		font-size: clamp(1.75rem, 4vw, 2.5rem);
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

	@media (max-width: 40rem) {
		.transcript-inner {
			padding: var(--space-6) var(--space-4);
		}

		.from-user {
			padding-inline-start: 0;
		}
	}
</style>
