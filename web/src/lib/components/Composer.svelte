<script lang="ts">
import { onDestroy, tick, untrack } from "svelte";
import {
	type Attachments,
	attachmentsOf,
	type Chip,
	IMAGE_TYPES,
	MAX_IMAGE_BYTES,
	MAX_IMAGES,
	MAX_MESSAGE_BYTES,
	messageBytes,
	PASTE_CHIP_LINES,
	readDataUrl,
} from "#lib/attachments.ts";
import {
	type PanelKind,
	parseComposerText,
	slashCommands,
} from "#lib/commands.ts";
import AttachmentChips from "#lib/components/AttachmentChips.svelte";
import CommandPalette from "#lib/components/CommandPalette.svelte";
import CompletionList from "#lib/components/CompletionList.svelte";
import FollowupQueue from "#lib/components/FollowupQueue.svelte";
import { handoffFocus } from "#lib/focus.ts";
import type { FileMatch, KeywordMatch } from "#lib/types.ts";

const INTERRUPT_GUARD_MS = 2000;
const FILE_SEARCH_DEBOUNCE_MS = 120;
const FILE_LIMIT = 20;

interface Mention {
	readonly start: number;
	readonly end: number;
	readonly query: string;
}

function mentionAt(text: string, caret: number): Mention | null {
	const head = text.slice(0, caret).match(/\S*$/)?.[0] ?? "";
	const tail = text.slice(caret).match(/^\S*/)?.[0] ?? "";
	const start = caret - head.length;
	const end = caret + tail.length;
	const token = text.slice(start, end);
	if (!token.startsWith("@")) return null;
	return { start, end, query: token.slice(1) };
}

interface Props {
	disabled: boolean;
	running: boolean;
	followups: readonly string[];
	history: readonly string[];
	onEditFollowup: (index: number) => string | undefined;
	onRemoveFollowup: (index: number) => void;
	onMatch?: (text: string) => Promise<readonly KeywordMatch[]>;
	onFileSearch?: (query: string) => Promise<readonly FileMatch[]>;
	onSend: (text: string, attachments: Attachments) => void;
	onSteer: (text: string) => void;
	onFollowup: (text: string) => void;
	onInterrupt: () => void;
	retrying?: boolean;
	onOpenPanel?: (kind: PanelKind) => void;
	acceptsImages?: boolean;
}

let {
	disabled,
	running,
	followups,
	history,
	onEditFollowup,
	onRemoveFollowup,
	onMatch,
	onFileSearch,
	onSend,
	onSteer,
	onFollowup,
	onInterrupt,
	retrying = false,
	onOpenPanel,
	acceptsImages = true,
}: Props = $props();
let text = $state("");
let error = $state("");
let matches = $state<readonly KeywordMatch[]>([]);
let browsing: number | null = null;
let savedDraft = "";
let matchSeq = 0;
let textarea = $state<HTMLTextAreaElement>();
let region = $state<HTMLElement>();
let guardUntil = $state(0);
let guardTimer: ReturnType<typeof setTimeout> | undefined;
let chips = $state<readonly Chip[]>([]);
let nextChipId = 0;
let pendingReads = 0;
const interruptArmed = $derived(running && guardUntil > 0);
const parsed = $derived(parseComposerText(text));
const commandQuery = $derived(parsed.kind === "command" ? parsed.query : null);
let paletteClosed = $state(false);
let paletteActive = $state(0);
const paletteOpen = $derived(commandQuery !== null && !paletteClosed);
const paletteId = "command-palette";
let caret = $state(0);
let mentionClosed = $state(false);
let fileMatches = $state<readonly FileMatch[]>([]);
let fileActive = $state(0);
let fileQuery = $state<string | null>(null);
let fileSeq = 0;
const fileListId = "file-completion";
const mention = $derived(mentionAt(text, Math.min(caret, text.length)));
const mentionQuery = $derived(mention?.query ?? null);
const fileOpen = $derived(
	!paletteOpen && mention !== null && !mentionClosed && fileMatches.length > 0,
);
const filePending = $derived(
	!paletteOpen &&
		mention !== null &&
		!mentionClosed &&
		onFileSearch !== undefined &&
		mention.query !== "" &&
		fileQuery !== mention.query,
);
const fileItems = $derived(
	fileMatches.slice(0, FILE_LIMIT).map((match) => ({
		id: match.path,
		primary: match.fileName,
		secondary: match.path,
	})),
);
const completionId = $derived(
	paletteOpen ? paletteId : fileOpen ? fileListId : undefined,
);
const activeCompletion = $derived(paletteOpen ? paletteActive : fileActive);

function disarm(): void {
	clearTimeout(guardTimer);
	guardTimer = undefined;
	guardUntil = 0;
}

function openPanel(kind: PanelKind): void {
	text = "";
	browsing = null;
	error = "";
	onOpenPanel?.(kind);
}

function submitCommand(query: string): void {
	const command = slashCommands.find((entry) => entry.name === query);
	if (command === undefined) {
		error = `No command named /${query}. Start with // to send a message that begins with /.`;
		return;
	}
	openPanel(command.panel);
}

$effect(() => {
	if (!running) disarm();
});

onDestroy(() => clearTimeout(guardTimer));

function pressEscape(): void {
	if (!running) return;
	if (retrying || Date.now() < guardUntil) {
		disarm();
		onInterrupt();
		return;
	}
	guardUntil = Date.now() + INTERRUPT_GUARD_MS;
	guardTimer = setTimeout(disarm, INTERRUPT_GUARD_MS);
}

function onWindowKeydown(event: KeyboardEvent): void {
	if (event.key !== "Escape" || event.defaultPrevented) return;
	const focused = document.activeElement;
	if (focused && focused !== document.body && !region?.contains(focused))
		return;
	pressEscape();
}

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

$effect(() => {
	const query = mentionQuery;
	const seq = ++fileSeq;
	fileMatches = [];
	fileActive = 0;
	fileQuery = null;
	const search = untrack(() => onFileSearch);
	if (!search || query === null || query === "") return;
	const timer = setTimeout(() => {
		search(query).then(
			(found) => {
				if (seq !== fileSeq) return;
				fileMatches = found;
				fileActive = 0;
				fileQuery = query;
			},
			() => {
				if (seq !== fileSeq) return;
				fileMatches = [];
				fileQuery = query;
			},
		);
	}, FILE_SEARCH_DEBOUNCE_MS);
	return () => clearTimeout(timer);
});

function syncCaret(field: HTMLTextAreaElement): void {
	caret = field.selectionStart;
}

function onFileKey(event: KeyboardEvent): boolean {
	const count = fileMatches.length;
	if (event.key === "ArrowDown" || event.key === "ArrowUp") {
		if (count === 0) return false;
		const step = event.key === "ArrowDown" ? 1 : -1;
		fileActive = (fileActive + step + count) % count;
		return true;
	}
	if (event.key === "Escape") {
		if (!fileOpen) return false;
		mentionClosed = true;
		return true;
	}
	const accepts =
		(event.key === "Enter" || event.key === "Tab") &&
		!event.shiftKey &&
		!event.altKey;
	if (!accepts) return false;
	insertFile(fileMatches[fileActive]);
	return true;
}

function insertFile(match: FileMatch | undefined): void {
	const current = mention;
	if (match === undefined || current === null) return;
	const insert = `@${match.path} `;
	text = text.slice(0, current.start) + insert + text.slice(current.end);
	const position = current.start + insert.length;
	caret = position;
	void tick().then(() => textarea?.setSelectionRange(position, position));
}

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

function clearDraft(): void {
	text = "";
	browsing = null;
}

function imageCount(): number {
	return chips.filter((chip) => chip.kind === "image").length;
}

function removeChip(id: number): void {
	chips = chips.filter((chip) => chip.id !== id);
}

async function attachImage(file: File): Promise<void> {
	if (!acceptsImages) {
		error = "this model does not accept images";
		return;
	}
	if (!IMAGE_TYPES.includes(file.type)) {
		error = `unsupported image type ${file.type}; send PNG, JPEG or GIF`;
		return;
	}
	if (file.size > MAX_IMAGE_BYTES) {
		error = "Image is larger than 5 MiB.";
		return;
	}
	if (imageCount() + pendingReads >= MAX_IMAGES) {
		error = "At most 10 images per message.";
		return;
	}
	error = "";
	pendingReads += 1;
	try {
		const dataUrl = await readDataUrl(file);
		chips = [
			...chips,
			{
				kind: "image",
				id: ++nextChipId,
				name: file.name,
				mime: file.type,
				size: file.size,
				dataUrl,
			},
		];
	} catch (failure) {
		error = `Cannot read image: ${failure instanceof Error ? failure.message : String(failure)}`;
	} finally {
		pendingReads -= 1;
	}
}

function onPaste(event: ClipboardEvent): void {
	const data = event.clipboardData;
	if (!data) return;
	const images = Array.from(data.files).filter((file) =>
		file.type.startsWith("image/"),
	);
	if (images.length > 0) {
		event.preventDefault();
		for (const file of images) void attachImage(file);
		return;
	}
	const pasted = data.getData("text/plain").replace(/\r\n?/g, "\n");
	const body = pasted.endsWith("\n") ? pasted.slice(0, -1) : pasted;
	const lines = body.split("\n").length;
	if (lines < PASTE_CHIP_LINES) return;
	event.preventDefault();
	chips = [...chips, { kind: "paste", id: ++nextChipId, text: pasted, lines }];
}

function sendDraft(): void {
	const typed = text.trim();
	const attachments = attachmentsOf(chips);
	if (
		typed === "" &&
		attachments.texts.length === 0 &&
		attachments.images.length === 0
	) {
		validText();
		return;
	}
	if (messageBytes(typed, attachments) > MAX_MESSAGE_BYTES) {
		error =
			"The message and images are too large to send together. Remove an image.";
		return;
	}
	error = "";
	onSend(typed, attachments);
	clearDraft();
	chips = [];
}

function submitPrimary(): void {
	if (commandQuery !== null) {
		submitCommand(commandQuery);
		return;
	}
	if (running) {
		const value = validText();
		if (!value) return;
		onSteer(value);
		clearDraft();
		return;
	}
	sendDraft();
}

function queueFollowup(): void {
	const value = validText();
	if (!value) return;
	onFollowup(value);
	clearDraft();
}

function editFollowup(index: number): void {
	const restored = onEditFollowup(index);
	if (restored !== undefined) {
		text = restored;
		paletteClosed = false;
	}
	textarea?.focus();
}

function showRecall(value: string): void {
	text = value;
	paletteClosed = false;
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
	if (event.defaultPrevented) return;
	if (event.isComposing) return;
	if ((fileOpen || filePending) && onFileKey(event)) {
		event.preventDefault();
		return;
	}
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

<svelte:window onkeydown={onWindowKeydown} />

<section class="composer" aria-label="Message composer" bind:this={region}>
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
			role={completionId === undefined ? undefined : "combobox"}
			aria-expanded={completionId === undefined ? undefined : "true"}
			aria-controls={completionId}
			aria-activedescendant={completionId === undefined ? undefined : `${completionId}-${activeCompletion}`}
			onkeydown={onKeydown}
			onpaste={onPaste}
			oninput={(event) => {
				syncCaret(event.currentTarget);
				browsing = null;
				paletteClosed = false;
				mentionClosed = false;
			}}
			onkeyup={(event) => syncCaret(event.currentTarget)}
			onclick={(event) => syncCaret(event.currentTarget)}
		></textarea>
		{#if paletteOpen}
			<CommandPalette
				id={paletteId}
				field={textarea}
				query={commandQuery ?? ""}
				bind:active={paletteActive}
				onOpen={openPanel}
				onClose={() => (paletteClosed = true)}
			/>
		{/if}
		{#if fileOpen}
			<CompletionList id={fileListId} label="Files" items={fileItems} active={fileActive} />
		{/if}
		<AttachmentChips {chips} onRemove={removeChip} />
		{#if matches.length > 0}
			<p class="keywords">Keywords: {matches.map((match) => match.word).join(", ")}</p>
		{/if}
		<div class="composer-meta">
			<p id="composer-help">
				{#if interruptArmed}
					Esc again to interrupt
				{:else}
					Enter to {running ? "steer" : "send"} · Shift+Enter for a new line{running ? " · Alt+Enter to queue" : ""}
				{/if}
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
