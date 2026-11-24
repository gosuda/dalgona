<script lang="ts">
import { untrack } from "svelte";
import { handoffFocus } from "#lib/focus.ts";
import { isImeKey } from "#lib/ime.ts";
import { oneLine } from "#lib/oneLine.ts";
import {
	answerError,
	correctedQuestion,
	correctionText,
} from "#lib/question.ts";
import { displayBlock, displayLine, distinctLines } from "#lib/text/display.ts";
import type {
	QuestionItem,
	QuestionRequest,
	QuestionSelection,
	QuestionSubmission,
} from "#lib/types.ts";

interface Props {
	request: QuestionRequest;
	position: number;
	total: number;
	subagent: string | null;
	submission: QuestionSubmission | null;
	onSubmit: (
		request: QuestionRequest,
		selections: readonly QuestionSelection[],
	) => void;
	onDismiss: (request: QuestionRequest) => void;
}

let {
	request,
	position,
	total,
	subagent,
	submission,
	onSubmit,
	onDismiss,
}: Props = $props();
let card = $state<HTMLElement>();
let heading = $state<HTMLElement>();
let details = $state<HTMLElement>();
let selections = $state<readonly QuestionSelection[]>([]);
let refusal = $state<{ readonly id: string; readonly message: string } | null>(
	null,
);

const empty: QuestionSelection = { picked: [], other: "" };
const title = $derived(
	request.questions.length > 1
		? "Answer these questions"
		: "Answer this question",
);
const awaiting = $derived(submission?.correction === null);
const correction = $derived(submission?.correction ?? null);
const corrected = $derived(
	correction === null ? null : correctedQuestion(correction.field),
);
const cardError = $derived(
	correction !== null &&
		!request.questions.some((question) => question.id === corrected) &&
		request.questions.every((_, index) =>
			sameSelection(selectionAt(index), submission?.selections[index] ?? empty),
		)
		? correctionText(correction.reason, correction.message)
		: null,
);

const lineStep = 48;

function pageStep(body: HTMLElement): number {
	return Math.max(lineStep, body.clientHeight - lineStep);
}

const scrolls: Readonly<Record<string, (body: HTMLElement) => void>> = {
	ArrowUp: (body) => body.scrollBy({ top: -lineStep, behavior: "auto" }),
	ArrowDown: (body) => body.scrollBy({ top: lineStep, behavior: "auto" }),
	PageUp: (body) => body.scrollBy({ top: -pageStep(body), behavior: "auto" }),
	PageDown: (body) => body.scrollBy({ top: pageStep(body), behavior: "auto" }),
	Home: (body) => body.scrollTo({ top: 0, behavior: "auto" }),
	End: (body) => body.scrollTo({ top: body.scrollHeight, behavior: "auto" }),
};

$effect(() => {
	void request.id;
	selections = untrack(() => submission?.selections ?? []);
	heading?.focus();
});

function selectionAt(index: number): QuestionSelection {
	return selections[index] ?? empty;
}

function update(index: number, next: QuestionSelection): void {
	const copy = request.questions.map((_, entry) => selectionAt(entry));
	copy[index] = next;
	selections = copy;
}

function choose(index: number, question: QuestionItem, label: string): void {
	const current = selectionAt(index);
	if (!question.multiSelect) {
		update(index, { picked: [label], other: "" });
		return;
	}
	const picked = current.picked.includes(label)
		? current.picked.filter((entry) => entry !== label)
		: [...current.picked, label];
	update(index, { picked, other: current.other });
}

function typeOther(index: number, question: QuestionItem, value: string): void {
	refusal = null;
	const current = selectionAt(index);
	const picked = question.multiSelect || value === "" ? current.picked : [];
	update(index, { picked, other: value });
}

function hasField(question: QuestionItem): boolean {
	return question.options.length === 0 || question.isOther;
}

function sameSelection(a: QuestionSelection, b: QuestionSelection): boolean {
	return (
		a.other === b.other &&
		a.picked.length === b.picked.length &&
		a.picked.every((label, index) => b.picked[index] === label)
	);
}

function errorFor(index: number, question: QuestionItem): string | null {
	const selection = selectionAt(index);
	const local = hasField(question) ? answerError(selection.other) : null;
	if (local !== null) return local;
	if (correction === null || corrected !== question.id) return null;
	const sent = submission?.selections[index] ?? empty;
	return sameSelection(selection, sent)
		? correctionText(correction.reason, correction.message)
		: null;
}

function previewOf(
	question: QuestionItem,
	selection: QuestionSelection,
): string | null {
	const chosen = question.options.filter(
		(option) =>
			selection.picked.includes(option.label) && option.preview !== null,
	);
	return chosen.at(-1)?.preview ?? null;
}

function submit(): void {
	if (awaiting || cardError !== null) return;
	if (request.questions.some((question, index) => errorFor(index, question)))
		return;
	onSubmit(
		request,
		request.questions.map((_, index) => selectionAt(index)),
	);
}

function skip(): void {
	if (awaiting) return;
	onSubmit(
		request,
		request.questions.map(() => empty),
	);
}

function dismiss(): void {
	if (awaiting) return;
	onDismiss(request);
}

function scrollDetails(event: KeyboardEvent, target: Node): void {
	const scroll = scrolls[event.key];
	if (scroll === undefined || details === undefined) return;
	if (event.ctrlKey || event.metaKey || event.altKey) return;
	if (details.contains(target)) return;
	if (details.scrollHeight <= details.clientHeight) return;
	event.preventDefault();
	scroll(details);
}

function onKeydown(event: KeyboardEvent): void {
	if (isImeKey(event)) return;
	if (!(event.target instanceof Node) || !card?.contains(event.target)) return;
	if (event.key !== "Escape") {
		scrollDetails(event, event.target);
		return;
	}
	event.preventDefault();
	dismiss();
}
</script>

<svelte:window onkeydown={onKeydown} />

<section
	class="question-card"
	aria-labelledby="question-title"
	bind:this={card}
	{@attach handoffFocus(() => document.getElementById("prompt") ?? undefined)}
>
	<p class="kicker">
		Question required{total > 1 ? ` · ${position} of ${total}` : ""}
	</p>
	<h2 id="question-title" tabindex="-1" bind:this={heading}>{title}</h2>
	<div
		class="question-body"
		role="region"
		aria-label="Question details"
		bind:this={details}
	>
		{#if subagent !== null}
			<p class="subagent">From subagent {subagent}</p>
		{/if}
		{#each request.questions as question, index (question.id)}
			{@const selection = selectionAt(index)}
			{@const error = refusal?.id === question.id ? refusal.message : errorFor(index, question)}
			{@const labels = distinctLines(question.options.map((option) => option.label))}
			<fieldset class="question" disabled={awaiting} aria-describedby={`question-prompt-${question.id}`}>
				<legend>{displayLine(question.header)}</legend>
				<p class="prompt" id={`question-prompt-${question.id}`}>{displayLine(question.question)}</p>
				{#each question.options as option, optionIndex (option.label)}
					<div class="option" class:selected={selection.picked.includes(option.label)}>
						<label>
							{#if question.multiSelect}
								<input
									type="checkbox"
									checked={selection.picked.includes(option.label)}
									onchange={() => choose(index, question, option.label)}
								/>
							{:else}
								<input
									type="radio"
									name={`${request.id}-${question.id}`}
									checked={selection.picked.includes(option.label)}
									onchange={() => choose(index, question, option.label)}
								/>
							{/if}
							<span class="choice">{labels[optionIndex]}</span>
							{#if optionIndex === 0}
								<span class="recommended">Recommended</span>
							{/if}
						</label>
						<p class="description">{displayLine(option.description)}</p>
					</div>
				{/each}
				{#if hasField(question)}
					<input
						class="other"
						type="text"
						placeholder={question.options.length === 0 ? "Answer" : "Other answer"}
						aria-label={question.options.length === 0 ? question.header : `Other answer for ${question.header}`}
						aria-invalid={error === null ? undefined : "true"}
						aria-describedby={error === null ? undefined : `question-error-${question.id}`}
						value={selection.other}
						oninput={(event) => typeOther(index, question, event.currentTarget.value)}
						{@attach oneLine((message) => (refusal = { id: question.id, message }))}
					/>
				{/if}
				{#if error !== null}
					<p class="field-error" id={`question-error-${question.id}`} role="alert">{error}</p>
				{/if}
				{#if previewOf(question, selection) !== null}
					<pre class="preview">{displayBlock(previewOf(question, selection) ?? "")}</pre>
				{/if}
			</fieldset>
		{/each}
	</div>
	{#if awaiting}
		<p class="sent" role="status">Answer sent. Waiting for the question to close.</p>
	{:else if cardError !== null}
		<p class="field-error" role="alert">{cardError}</p>
	{/if}
	<div class="request-actions">
		<button class="point" aria-disabled={awaiting ? "true" : undefined} onclick={submit}>Submit</button>
		<button class="quiet" aria-disabled={awaiting ? "true" : undefined} onclick={skip}>Skip</button>
		<button class="quiet" aria-disabled={awaiting ? "true" : undefined} onclick={dismiss}>Dismiss</button>
	</div>
</section>

<style>
	.question-card {
		display: flex;
		min-width: 0;
		flex-direction: column;
		margin: var(--space-4) var(--space-6) 0;
		border: var(--border-width) solid var(--line);
		border-radius: var(--radius-card);
		background: var(--surface);
		color: var(--text);
		padding: var(--space-6);
	}

	.kicker {
		margin: 0 0 var(--space-1);
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: var(--weight-title);
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	h2 {
		margin: 0;
		overflow-wrap: anywhere;
		font-family: var(--face-title);
		font-size: var(--size-title);
		font-weight: var(--weight-title);
		line-height: 1.25;
		text-wrap: balance;
	}

	.question-body {
		max-height: min(60dvh, 40rem);
		min-width: 0;
		margin-block-start: var(--space-4);
		overflow-y: auto;
		overscroll-behavior: contain;
	}

	.subagent {
		margin: 0 0 var(--space-3);
		color: var(--dim);
		font-size: var(--size-small);
		overflow-wrap: anywhere;
	}

	.question {
		min-width: 0;
		margin: 0 0 var(--space-4);
		border: 0;
		padding: 0;
	}

	legend {
		padding: 0;
		font-family: var(--face-title);
		font-size: var(--size-small);
		font-weight: var(--weight-title);
	}

	.prompt {
		margin: var(--space-2) 0;
		font-family: var(--face-text);
		font-size: var(--size-body);
		line-height: 1.5;
		overflow-wrap: anywhere;
	}

	.option {
		min-height: 54px;
		margin-block-end: var(--space-2);
		border: var(--border-width) solid var(--line);
		border-radius: 8px;
		padding: var(--space-2) var(--space-3);
	}

	.option.selected {
		background: var(--tone);
		border-inline-start: 2px solid var(--text);
	}

	.option label {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		font-family: var(--face-title);
		font-size: 14px;
		font-weight: var(--weight-title);
	}

	.recommended {
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: var(--weight-text);
	}

	.description {
		margin: var(--space-1) 0 0;
		color: var(--dim);
		font-size: var(--size-small);
		line-height: 1.4;
		overflow-wrap: anywhere;
	}

	.other {
		width: 100%;
		min-width: 0;
		border: 0;
		border-bottom: var(--border-width) solid var(--strong);
		background: transparent;
		color: var(--text);
		font-size: max(1rem, var(--size-body));
		padding: var(--space-1) 0;
	}

	.field-error {
		margin: var(--space-1) 0 0;
		color: var(--failed-text);
		font-size: var(--size-caption);
		overflow-wrap: anywhere;
	}

	.sent {
		margin: var(--space-3) 0 0;
		color: var(--dim);
		font-size: var(--size-caption);
	}

	.preview {
		margin: var(--space-2) 0 0;
		background: var(--tone);
		border-radius: var(--radius-control);
		padding: var(--space-3);
		font-family: var(--face-mono);
		font-size: var(--size-caption);
		line-height: 1.4;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	.request-actions {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: var(--space-2);
		margin-block-start: var(--space-6);
	}
</style>
