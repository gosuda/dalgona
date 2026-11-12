<script lang="ts">
import KeywordText from "#lib/components/KeywordText.svelte";
import { handoffFocus } from "#lib/focus.ts";
import { findKeywords } from "#lib/keywords.ts";
import type { KeywordDefinition } from "#lib/types.ts";

interface Props {
	disabled: boolean;
	running: boolean;
	queuedCount: number;
	keywords: readonly KeywordDefinition[];
	onSend: (text: string) => void;
	onSteer: (text: string) => void;
	onFollowup: (text: string) => void;
	onInterrupt: () => void;
}

let {
	disabled,
	running,
	queuedCount,
	keywords,
	onSend,
	onSteer,
	onFollowup,
	onInterrupt,
}: Props = $props();
let text = $state("");
let focused = $state(false);
let error = $state("");
let textarea = $state<HTMLTextAreaElement>();
let matches = $derived(findKeywords(text, keywords));

function validText(): string | null {
	const value = text.trim();
	if (value) {
		error = "";
		return value;
	}
	error = "Enter a message before sending.";
	textarea?.focus();
	return null;
}

function submitPrimary(): void {
	const value = validText();
	if (!value) return;
	if (running) {
		onSteer(value);
	} else {
		onSend(value);
	}
	text = "";
}

function queueFollowup(): void {
	const value = validText();
	if (!value) return;
	onFollowup(value);
	text = "";
}

function onKeydown(event: KeyboardEvent): void {
	if (event.isComposing || event.key !== "Enter") return;
	if ((event.metaKey || event.ctrlKey) && running) {
		event.preventDefault();
		queueFollowup();
	} else if (!event.shiftKey) {
		event.preventDefault();
		submitPrimary();
	}
}
</script>

<section class="composer" aria-label="Message composer">
	{#if matches.length > 0}
		<div class="keyword-row" aria-label="Active keywords">
			<span>Keywords</span>
			{#each matches as match, index (`${match.word}:${match.start}:${index}`)}
				<span class="keyword-chip"><KeywordText text={match.word} {keywords} active={focused} /></span>
			{/each}
		</div>
	{/if}

	<form
		onsubmit={(event) => {
			event.preventDefault();
			submitPrimary();
		}}
	>
		<label for="prompt">Message</label>
		<textarea
			bind:this={textarea}
			id="prompt"
			name="prompt"
			rows="3"
			bind:value={text}
			disabled={disabled}
			placeholder="Fix the failing test and verify the change"
			aria-invalid={error ? "true" : "false"}
			aria-describedby="composer-help composer-error"
			onkeydown={onKeydown}
			onfocus={() => (focused = true)}
			onblur={() => (focused = false)}
		></textarea>
		<div class="composer-meta">
			<p id="composer-help">
				Enter to {running ? "steer" : "send"} · Shift+Enter for a new line{running ? " · Ctrl+Enter to queue" : ""}
			</p>
			<p class="queued" role="status">{queuedCount > 0 ? `${queuedCount} follow-up${queuedCount === 1 ? "" : "s"} queued` : ""}</p>
		</div>
		<p id="composer-error" class="error" role="alert">{error}</p>

		<div class="actions">
			<button class={running ? "quiet" : "primary"} type="submit" disabled={disabled}>{running ? "Steer" : "Send"}</button>
			{#if running}
				<button type="button" disabled={disabled} onclick={queueFollowup} {@attach handoffFocus(() => textarea)}>Queue follow-up</button>
				<button class="quiet danger" type="button" disabled={disabled} onclick={onInterrupt} {@attach handoffFocus(() => textarea)}>Interrupt turn</button>
			{/if}
		</div>
	</form>
</section>

<style>
	.composer {
		border-block-start: 1px solid var(--color-border);
		background: var(--color-bg-surface);
		padding: var(--space-4) var(--space-5) var(--space-5);
	}

	form,
	.keyword-row {
		width: min(100%, 52rem);
		margin-inline: auto;
	}

	label {
		display: block;
		margin-block-end: var(--space-2);
		font-size: var(--text-body-small);
		font-weight: 650;
	}

	textarea {
		display: block;
		width: 100%;
		min-height: 6rem;
		max-height: 16rem;
		resize: vertical;
		border: 1px solid var(--color-border-strong);
		border-radius: var(--radius-medium);
		background: var(--color-bg-page);
		color: var(--color-text);
		caret-color: var(--color-accent);
		padding: var(--space-3);
		font-size: var(--text-body);
		line-height: 1.5;
	}

	textarea[aria-invalid="true"] {
		border-color: var(--color-danger);
	}

	.composer-meta {
		display: flex;
		justify-content: space-between;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.composer-meta p,
	.error {
		margin: var(--space-2) 0 0;
		color: var(--color-text-secondary);
		font-size: var(--text-caption);
	}

	.queued {
		font-variant-numeric: tabular-nums;
	}

	.error {
		min-height: 1.5em;
		color: var(--color-danger);
	}

	.actions {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--space-3);
		margin-block-start: var(--space-3);
	}

	.keyword-row {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--space-2);
		margin-block-end: var(--space-3);
		color: var(--color-text-secondary);
		font-size: var(--text-caption);
	}

	.keyword-chip {
		font-weight: 650;
	}

	@media (max-width: 40rem) {
		.composer {
			padding: var(--space-3) var(--space-4) var(--space-4);
		}

		.actions {
			gap: var(--space-2);
		}
	}
</style>
