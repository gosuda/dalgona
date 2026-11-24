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
	type ShellRun,
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
import { isImeKey } from "#lib/ime.ts";
import {
	faultNotices,
	isWhitespaceOnly,
	multilineText,
	selectDeletedCluster,
	textFault,
} from "#lib/inputtext.ts";
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
	fileSearchRoot: string | null;
	onFileSearch?: (query: string) => Promise<readonly FileMatch[]>;
	onSend: (text: string, attachments: Attachments) => void;
	onSteer: (text: string) => void;
	onFollowup: (text: string) => void;
	onInterrupt: () => void;
	threadOpen?: boolean;
	retrying?: boolean;
	deciding?: boolean;
	onOpenPanel?: (kind: PanelKind) => void;
	acceptsImages?: boolean;
	cwd?: string;
	onShell?: (command: string) => Promise<ShellRun | null> | null;
}

let {
	disabled,
	running,
	followups,
	history,
	onEditFollowup,
	onRemoveFollowup,
	onMatch,
	fileSearchRoot,
	onFileSearch,
	onSend,
	onSteer,
	onFollowup,
	onInterrupt,
	threadOpen = true,
	retrying = false,
	deciding = false,
	onOpenPanel,
	acceptsImages = true,
	cwd = "",
	onShell,
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
const commandQuery = $derived(
	parsed.kind === "command" && !parsed.query.includes(" ")
		? parsed.query
		: null,
);
let paletteClosed = $state(false);
let paletteActive = $state(0);
const paletteOpen = $derived(commandQuery !== null && !paletteClosed);
const paletteId = "command-palette";
let caret = $state(0);
let dismissedFileRoot = $state<string | null>(null);
let fileActive = $state(0);
let fileResult = $state<{
	root: string;
	query: string;
	matches: readonly FileMatch[];
} | null>(null);
let fileSeq = 0;
const fileListId = "file-completion";
const mention = $derived(
	parsed.kind === "command"
		? null
		: mentionAt(text, Math.min(caret, text.length)),
);
const mentionQuery = $derived(mention?.query ?? null);
const searchableQuery = $derived(
	mentionQuery !== null &&
		mentionQuery !== "" &&
		textFault(mentionQuery) === null
		? mentionQuery
		: null,
);
const searchRoot = $derived(
	disabled || !fileSearchRoot ? null : fileSearchRoot,
);
const mentionClosed = $derived(
	dismissedFileRoot !== null && dismissedFileRoot === fileSearchRoot,
);
const currentFileResult = $derived(
	fileResult?.root === searchRoot && fileResult.query === mentionQuery
		? fileResult
		: null,
);
const shownFileMatches = $derived(
	currentFileResult?.matches.slice(0, FILE_LIMIT) ?? [],
);
const fileOpen = $derived(
	!paletteOpen && !mentionClosed && shownFileMatches.length > 0,
);
const filePending = $derived(
	!paletteOpen &&
		!mentionClosed &&
		searchRoot !== null &&
		searchableQuery !== null &&
		onFileSearch !== undefined &&
		currentFileResult === null,
);
const fileItems = $derived(
	shownFileMatches.map((match) => ({
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
	if (event.key !== "Escape" || event.defaultPrevented || isImeKey(event))
		return;
	const focused = document.activeElement;
	if (focused && focused !== document.body && !region?.contains(focused))
		return;
	pressEscape();
}

$effect(() => {
	const draft = text;
	const seq = ++matchSeq;
	if (!onMatch || isWhitespaceOnly(draft) || textFault(draft) !== null) {
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
	if (fileSearchRoot && dismissedFileRoot !== fileSearchRoot) {
		dismissedFileRoot = null;
	}
});

$effect(() => {
	const root = searchRoot;
	const query = searchableQuery;
	const seq = ++fileSeq;
	fileResult = null;
	fileActive = 0;
	const search = untrack(() => onFileSearch);
	if (!root || !search || !query) return;
	const isCurrent = () =>
		seq === fileSeq && root === searchRoot && query === mentionQuery;
	const receive = (matches: readonly FileMatch[]): void => {
		if (!isCurrent()) return;
		fileResult = { root, query, matches };
	};
	const timer = setTimeout(() => {
		if (!isCurrent()) return;
		search(query).then(receive, () => receive([]));
	}, FILE_SEARCH_DEBOUNCE_MS);
	return () => clearTimeout(timer);
});

function syncCaret(field: HTMLTextAreaElement): void {
	caret = field.selectionStart;
}

function onFileKey(event: KeyboardEvent): boolean {
	const count = shownFileMatches.length;
	if (event.key === "ArrowDown" || event.key === "ArrowUp") {
		if (count === 0) return false;
		const step = event.key === "ArrowDown" ? 1 : -1;
		fileActive = (fileActive + step + count) % count;
		return true;
	}
	if (event.key === "Escape") {
		if (!fileOpen) return false;
		dismissedFileRoot = fileSearchRoot;
		return true;
	}
	const accepts =
		(event.key === "Enter" || event.key === "Tab") &&
		!event.shiftKey &&
		!event.altKey;
	if (!accepts) return false;
	insertFile(shownFileMatches[fileActive]);
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

function messageText(): string | null {
	return parsed.kind === "message" ? parsed.text : null;
}

function textRefusal(value: string, blankNotice: string): string | null {
	const fault = textFault(value);
	if (fault !== null) return faultNotices[fault];
	return isWhitespaceOnly(value) ? blankNotice : null;
}

function validText(): string | null {
	const value = messageText();
	if (value === null) return null;
	const refusal = textRefusal(value, "Enter a message before sending.");
	if (refusal === null) {
		error = "";
		return value;
	}
	error = refusal;
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
	const pasted = multilineText(data.getData("text/plain"));
	const fault = textFault(pasted);
	if (fault !== null) {
		event.preventDefault();
		error = faultNotices[fault];
		return;
	}
	const body = pasted.endsWith("\n") ? pasted.slice(0, -1) : pasted;
	const lines = body.split("\n").length;
	if (lines < PASTE_CHIP_LINES) return;
	event.preventDefault();
	chips = [...chips, { kind: "paste", id: ++nextChipId, text: pasted, lines }];
}

function hasAttachments(attachments: Attachments): boolean {
	return attachments.texts.length > 0 || attachments.images.length > 0;
}

function sendableBody(typed: string, attachments: Attachments): string | null {
	if (hasAttachments(attachments) && typed === "") {
		error = "";
		return "";
	}
	if (hasAttachments(attachments) && isWhitespaceOnly(typed)) {
		const fault = textFault(typed);
		if (fault !== null) {
			error = faultNotices[fault];
			textarea?.focus();
			return null;
		}
		error = "";
		return typed;
	}
	return validText();
}

function sendDraft(): void {
	const typed = messageText();
	if (typed === null) return;
	const attachments = attachmentsOf(chips);
	const body = sendableBody(typed, attachments);
	if (body === null) return;
	const assembled = [body, ...attachments.texts]
		.filter((part) => part !== "")
		.join("\n\n");
	const fault = textFault(assembled);
	if (fault !== null) {
		error = faultNotices[fault];
		textarea?.focus();
		return;
	}
	if (messageBytes(body, attachments) > MAX_MESSAGE_BYTES) {
		error =
			"The message and images are too large to send together. Remove an image.";
		return;
	}
	error = "";
	onSend(body, attachments);
	clearDraft();
	chips = [];
}

function runShell(command: string): void {
	const refusal = textRefusal(command, "Enter a command after !.");
	if (refusal !== null) {
		error = refusal;
		return;
	}
	error = "";
	const run = onShell?.(command);
	if (run === undefined || run === null) return;
	clearDraft();
	void run.then((finished) => {
		if (finished !== null) {
			chips = [...chips, { kind: "shell", id: ++nextChipId, ...finished }];
		}
	});
}

function submitPrimary(): void {
	if (parsed.kind === "shell") {
		void runShell(parsed.command);
		return;
	}
	if (parsed.kind === "command") {
		if (commandQuery !== null) submitCommand(commandQuery);
		else error = "";
		return;
	}
	if (running) {
		const value = validText();
		if (value === null) return;
		onSteer(value);
		clearDraft();
		return;
	}
	sendDraft();
}

function queueFollowup(): void {
	if (parsed.kind === "shell") {
		error = "Shell commands run with Enter";
		return;
	}
	if (parsed.kind === "command") return;
	const value = validText();
	if (value === null) return;
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
	if (isImeKey(event)) return;
	if ((fileOpen || filePending) && onFileKey(event)) {
		event.preventDefault();
		return;
	}
	if (recallFromKey(event)) {
		event.preventDefault();
		return;
	}
	if (textarea !== undefined) selectDeletedCluster(event, textarea);
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

<section class="composer" class:deciding aria-label="Message composer" bind:this={region}>
	<form
		onsubmit={(event) => {
			event.preventDefault();
			submitPrimary();
		}}
	>
		<FollowupQueue items={followups} onEdit={editFollowup} onRemove={onRemoveFollowup} />
		<label for="prompt">{parsed.kind === "shell" ? "Shell command" : "Message"}</label>
		<textarea
			bind:this={textarea}
			id="prompt"
			name="prompt"
			rows={deciding ? 1 : 3}
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
				if (!(event instanceof InputEvent && event.isComposing)) {
					text = multilineText(text);
				}
				syncCaret(event.currentTarget);
				browsing = null;
				paletteClosed = false;
				dismissedFileRoot = null;
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
				{:else if parsed.kind === "shell"}
					Enter runs the command in {cwd}. It is not sent to the agent.
				{:else}
					Enter to {running ? "steer" : "send"} · Shift+Enter for a new line{running ? " · Alt+Enter to queue" : ""}
				{/if}
			</p>
		</div>
		<p id="composer-error" class="error" role="alert">{error}</p>

		<div class="actions">
			<button class={running || deciding || !threadOpen ? "quiet" : "point"} type="submit" disabled={disabled}>{running ? "Steer" : "Send"}</button>
			{#if running}
				<button type="button" disabled={disabled} onclick={queueFollowup} {@attach handoffFocus(() => textarea)}>Queue follow-up</button>
				<button class="quiet danger" type="button" disabled={disabled} onclick={onInterrupt} {@attach handoffFocus(() => textarea)}>Interrupt</button>
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

	.composer.deciding {
		flex: 0 1 auto;
		min-height: calc(var(--control-height) * 2 + var(--space-2));
		max-height: 40dvh;
		overflow-y: auto;
		padding: var(--space-2) var(--space-4);
	}

	.deciding textarea {
		height: var(--control-height);
		min-height: var(--control-height);
		max-height: var(--control-height-coarse);
		resize: none;
		padding: var(--space-1) var(--space-2);
	}

	.deciding label {
		margin-block-end: var(--space-1);
	}

	.deciding .actions {
		gap: var(--space-2);
		margin-block-start: var(--space-2);
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
