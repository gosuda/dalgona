<script lang="ts">
import type { RowProps } from "#lib/components/rows/types.ts";
import StateTag from "#lib/components/StateTag.svelte";
import { formatDuration } from "#lib/format.ts";
import { stateKey } from "#lib/labels.ts";
import { itemDisplayStatus } from "#lib/reducer.ts";
import { fullPresentation, lastLines } from "#lib/verbosity.ts";

let {
	item,
	awaiting,
	presentation = fullPresentation,
	onOpen,
}: RowProps = $props();
</script>

{#if item.kind === "commandExecution"}
	{@const state = stateKey(itemDisplayStatus(item.status, item.id, awaiting))}
	<article class="tool-item">
		<header>
			<p class="command">$ {item.command}</p>
			<span class="meta">
				{#if state}
					<StateTag state={state} />
				{:else}
					<span class="item-status">{item.status}</span>
				{/if}
				{#if item.exitCode !== null}
					<span class="item-status">exit {item.exitCode}</span>
				{/if}
				{#if item.durationMs !== null}
					<span class="item-status">{formatDuration(item.durationMs)}</span>
				{/if}
			</span>
		</header>
		{#if presentation.mode !== "line" && presentation.mode !== "hidden" && item.output}
			{@const shown = presentation.mode === "tail" ? lastLines(item.output, presentation.tailLines) : null}
			<pre class="output">{shown ? shown.text : item.output}</pre>
			{#if shown && shown.total > presentation.tailLines}
				<p class="cut">Showing the last {presentation.tailLines} of {shown.total} lines. <button type="button" onclick={onOpen}>Show all</button></p>
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

	.command {
		margin: 0;
		border-radius: var(--radius-control);
		background: var(--tone);
		padding: var(--space-3);
		font-size: var(--size-small);
		line-height: 1.5;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
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

	.output {
		margin-block-start: var(--space-3);
		overflow: auto;
		overscroll-behavior: contain;
		color: var(--dim);
		font-size: var(--size-caption);
		line-height: 1.55;
	}
</style>
