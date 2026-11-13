<script lang="ts">
import type { RowProps } from "#lib/components/rows/types.ts";
import { fullPresentation, lastLines, lineSummary } from "#lib/verbosity.ts";

let {
	item,
	running,
	presentation = fullPresentation,
	onOpen,
}: RowProps = $props();
</script>

{#if item.kind === "reasoning"}
	{#if presentation.mode === "line"}
		<p class="summary">{lineSummary(item)}</p>
	{:else if presentation.mode === "tail"}
		<p class="tail">{lastLines(item.text, presentation.tailLines).text}</p>
	{:else}
		<details class="reasoning" open={!item.complete && running}>
			<summary>Thinking</summary>
			<p>{item.text}</p>
		</details>
	{/if}
	{#if presentation.mode !== "full" && onOpen}
		<button type="button" aria-label="Open full text" onclick={onOpen}>Open</button>
	{/if}
{/if}

<style>
	.reasoning {
		max-width: 70ch;
		color: var(--dim);
		font-size: var(--size-small);
	}

	.reasoning summary {
		color: var(--text);
		cursor: pointer;
		font-weight: 600;
	}

	.reasoning p {
		margin-block: var(--space-3) 0;
		white-space: pre-wrap;
	}
</style>
