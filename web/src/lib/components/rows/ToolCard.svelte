<script lang="ts">
import type { RowProps } from "#lib/components/rows/types.ts";
import StateTag from "#lib/components/StateTag.svelte";
import { formatDuration } from "#lib/format.ts";
import { stateKey } from "#lib/labels.ts";
import { itemDisplayStatus } from "#lib/reducer.ts";
import type { JsonValue } from "#lib/types.ts";
import { fullPresentation, lastLines, lineSummary } from "#lib/verbosity.ts";

let {
	item,
	awaiting,
	presentation = fullPresentation,
	onOpen,
}: RowProps = $props();

function argumentsText(value: JsonValue): string {
	return JSON.stringify(value, null, 2) ?? "";
}
</script>

{#if item.kind === "toolCall"}
	{@const state = stateKey(itemDisplayStatus(item.status, item.id, awaiting))}
	<article class="tool-item">
		<header>
			<span class="item-label">{item.name}</span>
			<span class="meta">
				{#if state}
					<StateTag state={state} />
				{:else}
					<span class="item-status">{item.status}</span>
				{/if}
				{#if item.durationMs !== null}
					<span class="item-status">{formatDuration(item.durationMs)}</span>
				{/if}
			</span>
		</header>
		{#if presentation.mode === "line"}
			<p class="summary">{lineSummary(item)}</p>
		{:else}
			{#if presentation.mode === "full"}
				<details>
					<summary>Arguments</summary>
					<pre class="output">{argumentsText(item.arguments)}</pre>
				</details>
			{/if}
			{#if item.output}
				{@const shown = presentation.mode === "tail" ? lastLines(item.output, presentation.tailLines) : null}
				<pre class="output">{shown ? shown.text : item.output}</pre>
				{#if shown && shown.total > presentation.tailLines}
					<p class="cut">Showing the last {presentation.tailLines} of {shown.total} lines. <button type="button" onclick={onOpen}>Show all</button></p>
				{/if}
			{/if}
		{/if}
		{#if presentation.mode !== "full" && onOpen}
			<button type="button" aria-label="Open full text" onclick={onOpen}>Open</button>
		{/if}
	</article>
{/if}

<style>
	.tool-item {
		min-width: 0;
		border: 1px solid var(--line);
		border-radius: var(--radius-tag);
		background: var(--surface);
		padding: var(--space-4);
	}

	.tool-item header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.item-label {
		display: block;
		margin-block-end: var(--space-2);
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 650;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	.meta {
		display: inline-flex;
		align-items: baseline;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.item-status {
		color: var(--dim);
		font-size: var(--size-caption);
		font-variant-numeric: tabular-nums;
	}

	.tool-item details {
		max-width: 70ch;
		color: var(--dim);
		font-size: var(--size-small);
	}

	.tool-item summary {
		color: var(--text);
		cursor: pointer;
		font-weight: 600;
	}

	.output {
		margin-block-start: var(--space-3);
		overflow: auto;
		overscroll-behavior: contain;
		color: var(--dim);
		font-size: var(--size-caption);
		line-height: 1.55;
	}
</style>
