<script lang="ts">
import { handoffFocus } from "#lib/focus.ts";
import { MAX_ANSWER } from "#lib/question.ts";
import type {
	QuestionItem,
	QuestionRequest,
	QuestionSelection,
} from "#lib/types.ts";

interface Props {
	request: QuestionRequest;
	position: number;
	total: number;
	subagent: string | null;
	onSubmit: (
		request: QuestionRequest,
		selections: readonly QuestionSelection[],
	) => void;
	onDismiss: (request: QuestionRequest) => void;
}

let { request, position, total, subagent, onSubmit, onDismiss }: Props =
	$props();
let card = $state<HTMLElement>();
let heading = $state<HTMLElement>();
let selections = $state<readonly QuestionSelection[]>([]);

const empty: QuestionSelection = { picked: [], other: "" };
const title = $derived(
	request.questions.length > 1
		? "Answer these questions"
		: "Answer this question",
);

$effect(() => {
	void request.id;
	selections = [];
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
	const current = selectionAt(index);
	const picked = question.multiSelect || value === "" ? current.picked : [];
	update(index, { picked, other: value });
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
	onSubmit(
		request,
		request.questions.map((_, index) => selectionAt(index)),
	);
}

function skip(): void {
	onSubmit(
		request,
		request.questions.map(() => empty),
	);
}

function dismiss(): void {
	onDismiss(request);
}

function onKeydown(event: KeyboardEvent): void {
	if (event.key !== "Escape") return;
	if (!(event.target instanceof Node) || !card?.contains(event.target)) return;
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
	<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
	<div class="question-body" role="region" aria-label="Question details" tabindex="0">
		{#if subagent !== null}
			<p class="subagent">From subagent {subagent}</p>
		{/if}
		{#each request.questions as question, index (question.id)}
			{@const selection = selectionAt(index)}
			<fieldset class="question">
				<legend>{question.header}</legend>
				<p class="prompt">{question.question}</p>
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
							<span class="choice">{option.label}</span>
							{#if optionIndex === 0}
								<span class="recommended">Recommended</span>
							{/if}
						</label>
						<p class="description">{option.description}</p>
					</div>
				{/each}
				{#if question.options.length === 0 || question.isOther}
					<input
						class="other"
						type="text"
						maxlength={MAX_ANSWER}
						placeholder={question.options.length === 0 ? "Answer" : "Other answer"}
						aria-label={question.options.length === 0 ? question.header : `Other answer for ${question.header}`}
						value={selection.other}
						oninput={(event) => typeOther(index, question, event.currentTarget.value)}
					/>
				{/if}
				{#if previewOf(question, selection) !== null}
					<pre class="preview">{previewOf(question, selection)}</pre>
				{/if}
			</fieldset>
		{/each}
	</div>
	<div class="request-actions">
		<button class="point" onclick={submit}>Submit</button>
		<button class="quiet" onclick={skip}>Skip</button>
		<button class="quiet" onclick={dismiss}>Dismiss</button>
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

	.question-body:focus-visible {
		outline: 2px solid var(--text);
		outline-offset: 2px;
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
