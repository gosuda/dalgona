<script lang="ts">
import type { ApprovalRequest, FileChange } from "#lib/types.ts";

type Decision = "accept" | "acceptForSession" | "decline" | "cancel";

interface Props {
	request: ApprovalRequest | null;
	changes: readonly FileChange[];
	onDecision: (request: ApprovalRequest, decision: Decision) => void;
}

let { request, changes, onDecision }: Props = $props();
let dialog = $state<HTMLDialogElement>();
let previousFocus = $state<HTMLElement | null>(null);

$effect(() => {
	const current = request;
	const requestId = current?.id;
	if (!dialog) return;
	if (current && !dialog.open) {
		previousFocus =
			document.activeElement instanceof HTMLElement
				? document.activeElement
				: null;
		dialog.showModal();
	}
	if (requestId !== undefined) {
		dialog.querySelector<HTMLElement>("#approval-title")?.focus();
	} else {
		if (dialog.open) dialog.close();
		previousFocus?.focus();
		previousFocus = null;
	}
});

function decide(decision: Decision): void {
	if (request) onDecision(request, decision);
}
</script>

<dialog
	bind:this={dialog}
	aria-labelledby="approval-title"
	aria-describedby="approval-description"
	oncancel={(event) => {
		event.preventDefault();
		decide("cancel");
	}}
>
	{#if request}
		<div class="approval-content">
			<p class="kicker">Approval required</p>
			<h2 id="approval-title" tabindex="-1">{request.title}</h2>
			<p id="approval-description">Review this action before the agent continues.</p>
			{#if request.detail}
				<pre>{request.detail}</pre>
			{/if}
			{#if request.reason}
				<p class="reason">{request.reason}</p>
			{/if}
			{#each changes as change (`${change.path}:${change.kind}`)}
				<details class="approval-change" open>
					<summary>{change.path} · {change.kind}</summary>
					<pre>{change.diff}</pre>
				</details>
			{/each}
			<p class="thread-reference">Thread <bdi>{request.threadId}</bdi></p>
			<div class="approval-actions">
				<button class="point" onclick={() => decide("accept")}>Accept action</button>
				<button onclick={() => decide("acceptForSession")}>Accept for session</button>
				<button class="quiet" onclick={() => decide("decline")}>Decline action</button>
				<button class="quiet" onclick={() => decide("cancel")}>Cancel</button>
			</div>
		</div>
	{/if}
</dialog>

<style>
	dialog {
		width: min(calc(100% - var(--space-8)), 36rem);
		max-height: calc(100dvh - var(--space-8));
		margin: auto;
		overflow-y: auto;
		overscroll-behavior: contain;
		border: 1px solid var(--strong);
		border-radius: var(--radius-card);
		background: var(--surface);
		color: var(--text);
		padding: 0;
	}

	dialog::backdrop {
		background: color-mix(in srgb, var(--canvas) 55%, transparent);
	}

	.approval-content {
		padding: var(--space-6);
	}

	.kicker {
		margin: 0 0 var(--space-2);
		color: var(--point);
		font-size: var(--size-caption);
		font-weight: 650;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	h2 {
		margin: 0;
		font-size: var(--size-heading);
		line-height: 1.25;
		text-wrap: balance;
	}

	#approval-description,
	.reason,
	.thread-reference {
		color: var(--dim);
		font-size: var(--size-small);
		text-wrap: pretty;
	}

	pre {
		max-height: 16rem;
		margin-block: var(--space-4);
		overflow: auto;
		border-radius: var(--radius-control);
		background: var(--tone);
		padding: var(--space-3);
		font-size: var(--size-caption);
		line-height: 1.5;
	}

	.thread-reference {
		overflow-wrap: break-word;
		font-size: var(--size-caption);
	}

	.approval-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-3);
		margin-block-start: var(--space-6);
	}
</style>
