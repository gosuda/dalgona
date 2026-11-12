<script lang="ts">
import type { RowProps } from "#lib/components/rows/types.ts";

let { item, running }: RowProps = $props();
</script>

{#if item.kind === "reasoning"}
	<details class="reasoning" open={!item.complete && running}>
		<summary>Thinking</summary>
		<p>{item.text}</p>
	</details>
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
