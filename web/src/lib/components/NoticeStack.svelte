<script lang="ts">
import { handoffFocus } from "#lib/focus.ts";
import { countLabel } from "#lib/labels.ts";
import type { Notice, TurnStatus, WaitingElsewhere } from "#lib/types.ts";

interface Props {
	notices: readonly Notice[];
	waiting: WaitingElsewhere;
	onDismiss: (id: number) => void;
	onOpenWaiting: (threadId: string) => void;
	turnStatus?: TurnStatus | null;
	turnError?: string | null;
	loadingTranscript?: boolean;
	retryRemainingMs?: number | null;
	awaitingApproval?: ReadonlySet<string>;
}

let {
	notices,
	waiting,
	onDismiss,
	onOpenWaiting,
	turnStatus = null,
	turnError = null,
	loadingTranscript = false,
	retryRemainingMs = null,
	awaitingApproval = new Set<string>(),
}: Props = $props();
let announcement = $derived.by(() => {
	if (loadingTranscript) return "Loading transcript…";
	if (turnStatus === "failed")
		return turnError ? `Failed: ${turnError}` : "Failed";
	if (turnStatus === "completed") return "Done";
	if (turnStatus === "interrupted") return "Stopped";
	if (retryRemainingMs !== null && retryRemainingMs > 0)
		return "Rate limited, retrying";
	if (awaitingApproval.size > 0) return "Waiting for your reply";
	if (turnStatus === "inProgress") return "Working";
	return "";
});
let errors = $derived(notices.filter((notice) => notice.tone === "error"));
let others = $derived(notices.filter((notice) => notice.tone !== "error"));
let waitingText = $derived(
	`${countLabel(waiting.count, "request", "requests")} waiting on another thread.`,
);

function focusMessage(): HTMLElement | undefined {
	return document.getElementById("prompt") ?? undefined;
}
</script>

<div class="notices" role="alert">
	{#each errors as notice (notice.id)}
		{@render row(notice)}
	{/each}
</div>
<div class="notices" role="status">
	<p class="sr-only">{announcement}</p>
	{#if waiting.count > 0}
		<div class="notice notice-info">
			<p>{waitingText}</p>
			{#if waiting.threadId}
				{@const threadId = waiting.threadId}
				<button class="quiet" onclick={() => onOpenWaiting(threadId)}>Open thread</button>
			{/if}
		</div>
	{/if}
	{#each others as notice (notice.id)}
		{@render row(notice)}
	{/each}
</div>

{#snippet row(notice: Notice)}
	<div class="notice notice-{notice.tone}">
		<p>{notice.text}</p>
		{#each notice.actions as action}
			<button class="quiet" onclick={action.run}>{action.label}</button>
		{/each}
		<button class="quiet" onclick={() => onDismiss(notice.id)} {@attach handoffFocus(focusMessage)}>Dismiss</button>
	</div>
{/snippet}

<style>
	.notices {
		min-width: 0;
	}

	.notice {
		display: flex;
		align-items: center;
		justify-content: space-between;
		flex-wrap: wrap;
		gap: var(--space-3);
		margin: 0 var(--space-6) var(--space-3);
		border-inline-start: 2px solid currentColor;
		padding: var(--space-3);
		font-size: var(--size-small);
	}

	.notice p {
		min-width: 0;
		margin: 0;
		overflow-wrap: anywhere;
	}

	.notice-error {
		color: var(--failed-text);
		font-weight: var(--weight-strong);
	}

	.notice-warning {
		color: var(--warning-text);
	}

	.notice-info {
		color: var(--dim);
	}

	.notice button {
		flex: none;
		min-height: 32px;
		padding: var(--space-1) var(--space-3);
		font-size: var(--size-caption);
	}

	@media (max-width: 40rem) {
		.notice {
			margin-inline: var(--space-4);
		}
	}
</style>
