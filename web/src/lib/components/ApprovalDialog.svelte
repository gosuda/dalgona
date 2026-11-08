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

$effect(() => {
	const current = request;
	const requestId = current?.id;
	if (!dialog) return;
	if (current && !dialog.open) dialog.showModal();
	if (requestId !== undefined) {
		dialog.querySelector<HTMLElement>("#approval-title")?.focus();
	} else if (!current && dialog.open) {
		dialog.close();
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
				<button class="primary" onclick={() => decide("accept")}>Accept action</button>
				<button onclick={() => decide("acceptForSession")}>Accept for session</button>
				<button class="quiet" onclick={() => decide("decline")}>Decline action</button>
			</div>
		</div>
	{/if}
</dialog>

<style>
	dialog {
		width: min(calc(100% - var(--space-6)), 36rem);
		max-height: calc(100dvh - var(--space-6));
		margin: auto;
		overflow-y: auto;
		overscroll-behavior: contain;
		border: 1px solid var(--color-border-strong);
		border-radius: var(--radius-large);
		background: var(--color-bg-surface);
		color: var(--color-text);
		padding: 0;
	}

	dialog::backdrop {
		background: oklch(0.19 0.018 65 / 0.55);
	}

	.approval-content {
		padding: var(--space-5);
	}

	.kicker {
		margin: 0 0 var(--space-2);
		color: var(--color-accent);
		font-size: var(--text-caption);
		font-weight: 650;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	h2 {
		margin: 0;
		font-size: var(--text-heading);
		line-height: 1.25;
		text-wrap: balance;
	}

	#approval-description,
	.reason,
	.thread-reference {
		color: var(--color-text-secondary);
		font-size: var(--text-body-small);
		text-wrap: pretty;
	}

	pre {
		max-height: 16rem;
		margin-block: var(--space-4);
		overflow: auto;
		border-radius: var(--radius-small);
		background: var(--color-bg-inset);
		padding: var(--space-3);
		font-size: var(--text-caption);
		line-height: 1.5;
	}

	.thread-reference {
		overflow-wrap: break-word;
		font-size: var(--text-caption);
	}

	.approval-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-3);
		margin-block-start: var(--space-5);
	}
</style>
