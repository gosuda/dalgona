<script lang="ts">
import type { RowProps } from "#lib/components/rows/types.ts";

let { item }: RowProps = $props();
</script>

{#if item.kind === "advisorNote"}
	<div class="advisor-note" data-severity={item.severity}>
		<span class="advisor-label">{item.advisor === "advisor" ? "advisor" : `advisor ${item.advisor}`} · {item.severity}:</span>
		{item.note}
	</div>
{/if}

<style>
	.advisor-note {
		max-width: 70ch;
		border-inline-start: 2px solid var(--strong);
		padding: var(--space-1) var(--space-3);
		color: var(--dim);
		font-size: var(--size-small);
		overflow-wrap: break-word;
		white-space: pre-wrap;
	}

	.advisor-note[data-severity="concern"] {
		border-inline-start-color: var(--accent-text);
	}

	.advisor-note[data-severity="blocker"] {
		border-inline-start-color: var(--failed-text);
	}

	.advisor-label {
		font-weight: 650;
	}
</style>
