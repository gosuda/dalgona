<script lang="ts">
import DiffCard from "#lib/components/DiffCard.svelte";
import Markdown from "#lib/components/Markdown.svelte";
import Panel from "#lib/components/panels/Panel.svelte";
import { formatDuration } from "#lib/format.ts";
import { itemStatusLabel } from "#lib/labels.ts";
import type { JsonValue, OutputPage, TranscriptItem } from "#lib/types.ts";
import { evalCode, isToolGroup, type TranscriptEntry } from "#lib/verbosity.ts";

interface Props {
	readonly target: TranscriptEntry | null;
	readonly readOutput: (itemId: string, offset: number) => Promise<OutputPage>;
	readonly onClose: () => void;
}

let { target, readOutput, onClose }: Props = $props();

const members = $derived(
	target === null ? [] : isToolGroup(target) ? target.items : [target],
);

let spilled = $state<Readonly<Record<string, string>>>({});
let spillErrors = $state<Readonly<Record<string, string>>>({});
const loading = new Set<string>();

async function loadSpill(itemId: string): Promise<void> {
	let text = "";
	let offset = 0;
	try {
		for (;;) {
			const page = await readOutput(itemId, offset);
			text += page.text;
			if (page.next >= page.total || page.next <= offset) break;
			offset = page.next;
		}
		spilled = { ...spilled, [itemId]: text };
	} catch (error) {
		spillErrors = {
			...spillErrors,
			[itemId]: error instanceof Error ? error.message : String(error),
		};
	} finally {
		loading.delete(itemId);
	}
}

$effect(() => {
	for (const item of members) {
		if (item.kind !== "commandExecution" && item.kind !== "toolCall") continue;
		if (item.spill === undefined) continue;
		if (spilled[item.id] !== undefined || loading.has(item.id)) continue;
		loading.add(item.id);
		void loadSpill(item.id);
	}
});

function outputOf(item: TranscriptItem): string {
	if (item.kind !== "commandExecution" && item.kind !== "toolCall") return "";
	if (item.spill === undefined) return item.output;
	const loaded = spilled[item.id];
	if (loaded !== undefined) return loaded;
	const failure = spillErrors[item.id];
	if (failure !== undefined)
		return `Could not load the full output: ${failure}`;
	return "Loading the full output…";
}

function argumentsText(value: JsonValue): string {
	return JSON.stringify(value, null, 2) ?? "";
}
</script>

<Panel title="Full text" open={target !== null} {onClose}>
	{#if target !== null}
		{#each members as item (item.id)}
			<section class="full-item">
				{#if item.kind === "commandExecution"}
					<p class="command">$ {item.command}</p>
					<p class="meta">
						{itemStatusLabel(item.status)}{item.exitCode !== null ? ` · exit ${item.exitCode}` : ""}{item.durationMs !== null ? ` · ${formatDuration(item.durationMs)}` : ""}
					</p>
					<pre>{outputOf(item)}</pre>
				{:else if item.kind === "toolCall"}
					<p class="label">{item.name}</p>
					{#if item.name === "eval"}
						<pre>{evalCode(item.arguments)}</pre>
					{:else}
						<pre>{argumentsText(item.arguments)}</pre>
					{/if}
					{#if outputOf(item) !== ""}
						<pre>{outputOf(item)}</pre>
					{/if}
				{:else if item.kind === "fileChange"}
					{#each item.changes as change, index (`${index}:${change.path}`)}
						<DiffCard {change} open limit={Number.POSITIVE_INFINITY} />
					{/each}
				{:else if item.kind === "reasoning"}
					<p class="reasoning">{item.text}</p>
				{:else if item.kind === "agentMessage"}
					<Markdown text={item.text} streaming={false} />
				{:else if item.kind === "userMessage"}
					<p>{item.text}</p>
				{/if}
			</section>
		{/each}
	{/if}
	{#snippet footer()}
		<button type="button" onclick={onClose}>Close</button>
	{/snippet}
</Panel>

<style>
	.full-item {
		display: grid;
		gap: var(--space-3);
		margin-block-end: var(--space-4);
		min-width: 0;
	}

	.command {
		margin: 0;
		font-family: var(--face-mono);
		overflow-wrap: anywhere;
	}

	.meta,
	.label {
		margin: 0;
		color: var(--dim);
		font-size: var(--size-caption);
	}

	pre {
		margin: 0;
		overflow-x: auto;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		color: var(--text);
		font-family: var(--face-mono);
		font-size: var(--size-caption);
		line-height: var(--leading-code);
	}

	.reasoning {
		margin: 0;
		white-space: pre-wrap;
		color: var(--dim);
	}
</style>
