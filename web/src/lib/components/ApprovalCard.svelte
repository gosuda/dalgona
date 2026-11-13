<script lang="ts">
import DiffCard from "#lib/components/DiffCard.svelte";
import { handoffFocus } from "#lib/focus.ts";
import type { ApprovalRequest, FileChange } from "#lib/types.ts";

type Decision = "accept" | "acceptForSession" | "decline" | "cancel";

interface Props {
	request: ApprovalRequest;
	changes: readonly FileChange[];
	position: number;
	total: number;
	subagent: string | null;
	onDecision: (request: ApprovalRequest, decision: Decision) => void;
}

let { request, changes, position, total, subagent, onDecision }: Props =
	$props();
let card = $state<HTMLElement>();
let heading = $state<HTMLElement>();
let details = $state<HTMLElement>();

const shortcuts: Readonly<Record<string, Decision>> = {
	y: "accept",
	a: "acceptForSession",
	n: "decline",
	Escape: "cancel",
};

const lineStep = 48;

function pageStep(body: HTMLElement): number {
	return Math.max(lineStep, body.clientHeight - lineStep);
}

const scrolls: Readonly<Record<string, (body: HTMLElement) => void>> = {
	ArrowUp: (body) => body.scrollBy({ top: -lineStep, behavior: "auto" }),
	ArrowDown: (body) => body.scrollBy({ top: lineStep, behavior: "auto" }),
	PageUp: (body) => body.scrollBy({ top: -pageStep(body), behavior: "auto" }),
	PageDown: (body) => body.scrollBy({ top: pageStep(body), behavior: "auto" }),
	Home: (body) => body.scrollTo({ top: 0, behavior: "auto" }),
	End: (body) => body.scrollTo({ top: body.scrollHeight, behavior: "auto" }),
};

const title = $derived(
	request.method === "item/fileChange/requestApproval"
		? "Apply these changes?"
		: "Run this command?",
);

$effect(() => {
	void request.id;
	heading?.focus();
});

function decide(decision: Decision): void {
	onDecision(request, decision);
}

function isEditable(target: EventTarget | null): boolean {
	return (
		target instanceof HTMLElement &&
		target.matches("input, textarea, select, [contenteditable]")
	);
}

function scrollDetails(event: KeyboardEvent, target: Node): void {
	const scroll = scrolls[event.key];
	if (scroll === undefined || details === undefined) return;
	if (details.contains(target)) return;
	if (details.scrollHeight <= details.clientHeight) return;
	event.preventDefault();
	scroll(details);
}

function onKeydown(event: KeyboardEvent): void {
	if (!(event.target instanceof Node) || !card?.contains(event.target)) return;
	if (event.ctrlKey || event.metaKey || event.altKey) return;
	if (isEditable(event.target)) return;
	const decision = shortcuts[event.key];
	if (decision === undefined) {
		scrollDetails(event, event.target);
		return;
	}
	event.preventDefault();
	decide(decision);
}
</script>

<svelte:window onkeydown={onKeydown} />

<section
	class="request-card"
	aria-labelledby="request-title"
	bind:this={card}
	{@attach handoffFocus(() => document.getElementById("prompt") ?? undefined)}
>
	<p class="kicker">
		Approval required{total > 1 ? ` · ${position} of ${total}` : ""}
	</p>
	<h2 id="request-title" tabindex="-1" bind:this={heading}>{title}</h2>
	<div
		class="request-body"
		role="region"
		aria-label="Request details"
		bind:this={details}
	>
		{#if request.detail}
			<pre class="command">{request.detail}</pre>
		{/if}
		{#if request.reason}
			<p class="reason">{request.reason}</p>
		{/if}
		{#if subagent !== null}
			<p class="subagent">From subagent {subagent}</p>
		{/if}
		{#each changes as change (`${change.path}:${change.kind}`)}
			<DiffCard {change} open={true} />
		{/each}
	</div>
	<div class="request-actions">
		<button class="point" onclick={() => decide("accept")}>Accept</button>
		<button onclick={() => decide("acceptForSession")}>Accept for session</button>
		<button class="quiet" onclick={() => decide("decline")}>Decline</button>
		<button class="quiet" onclick={() => decide("cancel")}>Cancel and interrupt</button>
	</div>
</section>

<style>
	.request-card {
		display: flex;
		flex: 0 0 auto;
		min-width: 0;
		min-height: 0;
		flex-direction: column;
		margin: var(--space-4) var(--space-6) 0;
		border: var(--border-width) solid var(--line);
		border-radius: var(--radius-card);
		background: var(--surface);
		color: var(--text);
		padding: var(--space-6);
	}

	.kicker {
		margin: 0 0 var(--space-1);
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: var(--weight-title);
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	h2 {
		margin: 0;
		overflow-wrap: anywhere;
		font-family: var(--face-title);
		font-size: var(--size-title);
		font-weight: var(--weight-title);
		line-height: 1.25;
		text-wrap: balance;
	}

	.request-body {
		flex: 1 0 auto;
		height: min(18dvh, 12rem);
		max-height: min(60dvh, 40rem);
		min-height: calc(var(--control-height) + var(--control-height) + var(--space-6));
		min-width: 0;
		margin-block-start: var(--space-4);
		overflow-y: auto;
		overscroll-behavior: contain;
	}

	.command {
		min-height: 7rem;
		margin: var(--space-4) 0;
		border-inline-start: 4px solid var(--text);
		border-radius: var(--radius-control);
		background: var(--tone);
		padding: var(--space-4) var(--space-4);
		font-family: var(--face-mono);
		font-size: var(--size-display);
		font-weight: var(--weight-title);
		line-height: 1.25;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	.reason {
		margin: var(--space-4) 0 0;
		font-family: var(--face-text);
		font-size: var(--size-body);
		line-height: 1.5;
		overflow-wrap: anywhere;
	}

	.subagent {
		margin: var(--space-3) 0 0;
		color: var(--dim);
		font-size: var(--size-small);
		overflow-wrap: anywhere;
	}

	.request-actions {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--space-2);
		margin-block-start: var(--space-6);
	}

	@media (max-width: 52rem) {
		.request-card {
			margin: var(--space-2) var(--space-4) 0;
			padding: var(--space-3);
		}

		h2 {
			font-size: var(--size-heading);
		}

		.request-body {
			margin-block-start: var(--space-2);
		}

		.request-actions {
			margin-block-start: var(--space-2);
		}
	}
</style>
