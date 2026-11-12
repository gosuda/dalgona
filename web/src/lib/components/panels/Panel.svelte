<script lang="ts">
import type { Snippet } from "svelte";

interface Props {
	readonly title: string;
	readonly open: boolean;
	readonly onClose: () => void;
	readonly children: Snippet;
	readonly footer?: Snippet;
}

let { title, open, onClose, children, footer }: Props = $props();
const titleId = $props.id();
let dialog = $state<HTMLDialogElement>();
let opener: HTMLElement | null = null;

$effect(() => {
	const node = dialog;
	if (node === undefined) return;
	if (open && !node.open) {
		opener =
			document.activeElement instanceof HTMLElement
				? document.activeElement
				: null;
		node.showModal();
		(
			node.querySelector<HTMLElement>(
				".panel-body button, .panel-body a[href], .panel-body input, .panel-body select, .panel-body textarea, .panel-body summary, .panel-body [tabindex]:not([tabindex='-1'])",
			) ?? node
		)?.focus();
	} else if (!open && node.open) {
		node.close();
		opener?.focus();
		opener = null;
	}
});

function onKey(event: KeyboardEvent): void {
	if (event.key !== "Escape") return;
	event.preventDefault();
	onClose();
}

function onCancel(event: Event): void {
	event.preventDefault();
	onClose();
}
</script>

<dialog
	bind:this={dialog}
	class="panel"
	aria-labelledby={titleId}
	onkeydown={onKey}
	oncancel={onCancel}
>
	<header class="panel-head">
		<h2 id={titleId}>{title}</h2>
	</header>
	<div class="panel-body">
		{@render children()}
	</div>
	{#if footer}
		<footer class="panel-foot">
			{@render footer()}
		</footer>
	{/if}
</dialog>

<style>
	.panel {
		box-sizing: border-box;
		width: min(70rem, calc(100vw - var(--space-8)));
		max-width: 100%;
		padding: var(--space-4);
		border: var(--border-width) solid var(--line);
		border-radius: var(--radius-card);
		background: var(--surface);
		color: var(--text);
	}

	.panel::backdrop {
		background: var(--canvas);
		opacity: 0.8;
	}

	.panel-head h2 {
		margin: 0 0 var(--space-3);
		font-family: var(--face-title);
		font-size: var(--size-title);
		font-weight: var(--weight-title);
	}

	.panel-body {
		max-height: calc(100dvh - var(--space-8));
		overflow-y: auto;
		overscroll-behavior: contain;
	}

	.panel-foot {
		display: flex;
		justify-content: flex-end;
		gap: var(--space-3);
		margin-block-start: var(--space-3);
	}
</style>
