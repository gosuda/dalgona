<script lang="ts">
import { handoffFocus } from "#lib/focus.ts";
import type { Notice } from "#lib/types.ts";

interface Props {
	notices: readonly Notice[];
	onDismiss: (id: number) => void;
}

let { notices, onDismiss }: Props = $props();
let errors = $derived(notices.filter((notice) => notice.tone === "error"));
let others = $derived(notices.filter((notice) => notice.tone !== "error"));

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
