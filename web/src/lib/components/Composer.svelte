<script lang="ts">
import { tick } from "svelte";
import FollowupQueue from "#lib/components/FollowupQueue.svelte";
import { handoffFocus } from "#lib/focus.ts";
import type { KeywordMatch } from "#lib/types.ts";

interface Props {
	disabled: boolean;
	running: boolean;
	followups: readonly string[];
	history: readonly string[];
	onEditFollowup: (index: number) => string | undefined;
	onRemoveFollowup: (index: number) => void;
	onMatch?: (text: string) => Promise<readonly KeywordMatch[]>;
	onSend: (text: string) => void;
	onSteer: (text: string) => void;
	onFollowup: (text: string) => void;
	onInterrupt: () => void;
}

let {
	disabled,
	running,
	followups,
	history,
	onEditFollowup,
	onRemoveFollowup,
	onMatch,
	onSend,
	onSteer,
	onFollowup,
	onInterrupt,
}: Props = $props();
let text = $state("");
let error = $state("");
let matches = $state<readonly KeywordMatch[]>([]);
let browsing: number | null = null;
let savedDraft = "";
let matchSeq = 0;
let textarea = $state<HTMLTextAreaElement>();

$effect(() => {
	const draft = text;
	const seq = ++matchSeq;
	if (!onMatch || draft.trim() === "") {
		matches = [];
		return;
	}
	onMatch(draft).then(
		(found) => {
			if (seq === matchSeq) matches = found;
		},
		() => {
			if (seq === matchSeq) matches = [];
		},
	);
});

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
	browsing = null;
}

function queueFollowup(): void {
	const value = validText();
	if (!value) return;
	onFollowup(value);
	text = "";
	browsing = null;
}

function editFollowup(index: number): void {
	const restored = onEditFollowup(index);
	if (restored !== undefined) text = restored;
	textarea?.focus();
}

function showRecall(value: string): void {
	text = value;
	void tick().then(() => textarea?.setSelectionRange(0, 0));
}

function recallOlder(field: HTMLTextAreaElement): boolean {
	const older = browsing === null ? history.length - 1 : browsing - 1;
	if (older < 0) return false;
	if (browsing === null) savedDraft = field.value;
	browsing = older;
	showRecall(history[older] ?? "");
	return true;
}

function recallNewer(): boolean {
	if (browsing === null) return false;
	const newer = browsing + 1;
	browsing = newer < history.length ? newer : null;
	showRecall(browsing === null ? savedDraft : (history[newer] ?? ""));
	return true;
}

function recallFromKey(event: KeyboardEvent): boolean {
	const field = event.currentTarget as HTMLTextAreaElement;
	if (field.selectionStart !== field.selectionEnd) return false;
	if (event.key === "ArrowUp" && field.selectionStart === 0)
		return recallOlder(field);
	if (
		event.key === "ArrowDown" &&
		(browsing !== null || field.selectionStart === text.length)
	)
		return recallNewer();
	return false;
}

function onKeydown(event: KeyboardEvent): void {
	if (event.isComposing) return;
	if (recallFromKey(event)) {
		event.preventDefault();
		return;
	}
	if (event.key !== "Enter") return;
	if (event.altKey && running) {
		event.preventDefault();
		queueFollowup();
	} else if (!event.shiftKey) {
		event.preventDefault();
		submitPrimary();
	}
}
</script>

<section class="composer" aria-label="Message composer">
	<form
		onsubmit={(event) => {
			event.preventDefault();
			submitPrimary();
		}}
	>
		<FollowupQueue items={followups} onEdit={editFollowup} onRemove={onRemoveFollowup} />
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
			oninput={() => (browsing = null)}
		></textarea>
		{#if matches.length > 0}
			<p class="keywords">Keywords: {matches.map((match) => match.word).join(", ")}</p>
		{/if}
		<div class="composer-meta">
			<p id="composer-help">
				Enter to {running ? "steer" : "send"} · Shift+Enter for a new line{running ? " · Alt+Enter to queue" : ""}
			</p>
		</div>
		<p id="composer-error" class="error" role="alert">{error}</p>

		<div class="actions">
			<button class={running ? "quiet" : "point"} type="submit" disabled={disabled}>{running ? "Steer" : "Send"}</button>
			{#if running}
				<button type="button" disabled={disabled} onclick={queueFollowup} {@attach handoffFocus(() => textarea)}>Queue follow-up</button>
				<button class="quiet danger" type="button" disabled={disabled} onclick={onInterrupt} {@attach handoffFocus(() => textarea)}>Interrupt turn</button>
			{/if}
		</div>
	</form>
</section>

<style>
	.composer {
		border-block-start: 1px solid var(--line);
		background: var(--surface);
		padding: var(--space-4) var(--space-6) var(--space-6);
	}

	form {
		width: min(100%, 52rem);
		margin-inline: auto;
	}

	label {
		display: block;
		margin-block-end: var(--space-2);
		font-size: var(--size-small);
		font-weight: 650;
	}

	textarea {
		display: block;
		width: 100%;
		min-height: 6rem;
		max-height: 16rem;
		resize: vertical;
		border: 1px solid var(--strong);
		border-radius: 14px;
		background: var(--canvas);
		color: var(--text);
		caret-color: var(--accent-text);
		padding: var(--space-3);
		line-height: 1.5;
	}

	textarea[aria-invalid="true"] {
		border-color: var(--failed-text);
	}

	.keywords {
		margin: var(--space-2) 0 0;
		color: var(--dim);
		font-size: var(--size-caption);
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
		color: var(--dim);
		font-size: var(--size-caption);
	}

	.error {
		min-height: 1.5em;
		color: var(--failed-text);
	}

	.actions {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--space-3);
		margin-block-start: var(--space-3);
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
