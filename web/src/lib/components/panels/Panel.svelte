<script lang="ts">
import type { Snippet } from "svelte";
import { messageOrTranscript } from "#lib/focus.ts";
import { isImeKey } from "#lib/ime.ts";

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
let composing = $state(false);
let heldEscape = false;
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
		if (
			opener !== null &&
			opener !== document.body &&
			opener.isConnected &&
			!opener.matches(":disabled")
		) {
			opener.focus();
		} else {
			const fallback = messageOrTranscript();
			fallback?.focus({ preventScroll: fallback.id === "transcript" });
		}
		opener = null;
	}
});

function onKey(event: KeyboardEvent): void {
	if (event.key !== "Escape") return;
	event.preventDefault();
	if (isImeKey(event)) heldEscape = true;
	if (composing || isImeKey(event)) return;
	onClose();
}

function onRelease(event: KeyboardEvent): void {
	if (event.key === "Escape") heldEscape = false;
}

function onCancel(event: Event): void {
	event.preventDefault();
	if (composing || heldEscape) return;
	onClose();
}
</script>

<dialog
	bind:this={dialog}
	class="panel"
	aria-labelledby={titleId}
	onkeydown={onKey}
	onkeyup={onRelease}
	oncancel={onCancel}
	oncompositionstart={() => (composing = true)}
	oncompositionend={() => (composing = false)}
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
		max-height: calc(100dvh - var(--space-8));
		padding: var(--space-4);
		border: var(--border-width) solid var(--line);
		border-radius: var(--radius-card);
		background: var(--surface);
		color: var(--text);
	}

	.panel[open] {
		display: flex;
		flex-direction: column;
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
		flex: 0 1 auto;
		min-height: 0;
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
