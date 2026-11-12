<script lang="ts">
import { type DiffLine, parseDiff } from "#lib/diff.ts";
import type { FileChange } from "#lib/types.ts";

interface Props {
	readonly change: FileChange;
	readonly open: boolean;
	readonly limit?: number;
	readonly tail?: boolean;
}

interface DiffRow {
	readonly kind: DiffLine["kind"] | "more";
	readonly text: string;
}

let { change, open, limit = 60, tail = false }: Props = $props();

const kindWords: Readonly<Record<string, string>> = {
	add: "Added",
	update: "Changed",
	delete: "Deleted",
};
const kindWord = $derived(kindWords[change.kind] ?? change.kind);
const lines = $derived(parseDiff(change.diff));
const kept = $derived(
	tail ? lines.slice(Math.max(0, lines.length - limit)) : lines.slice(0, limit),
);
const hidden = $derived(lines.length - kept.length);
const note = $derived(`… ${hidden} more ${hidden === 1 ? "line" : "lines"}`);
const rows = $derived.by((): readonly DiffRow[] => {
	if (hidden === 0) return kept;
	const more: DiffRow = { kind: "more", text: note };
	return tail ? [more, ...kept] : [...kept, more];
});
const counted = $derived(change.additions !== undefined);
</script>

<details class="file-change" open={open}>
	<summary>
		<span class="file-path">{change.path}</span>
		<span class="kind">{kindWord}</span>
		{#if counted}
			<span class="counts"><span class="added">+{change.additions}</span>{#if change.deletions !== undefined}{" "}<span class="removed">−{change.deletions}</span>{/if}</span>
		{/if}
	</summary>
	{#if change.diffOmitted}
		<p class="omitted">Diff not shown.</p>
	{:else}
		<pre class="diff">{#each rows as row, index (index)}{#if index > 0}{"\n"}{/if}{#if row.kind === "add"}<ins>{row.text}</ins>{:else if row.kind === "del"}<del>{row.text}</del>{:else if row.kind === "hunk"}<span class="hunk">{row.text}</span>{:else if row.kind === "more"}<span class="more">{row.text}</span>{:else}{row.text}{/if}{/each}</pre>
	{/if}
</details>

<style>
	.file-change {
		min-width: 0;
		max-width: 70ch;
		margin-block-start: var(--space-3);
		color: var(--dim);
		font-size: var(--size-small);
	}

	.file-change summary {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: var(--space-2);
		color: var(--text);
		cursor: pointer;
		font-weight: 600;
	}

	.file-path {
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.kind {
		color: var(--dim);
		font-size: var(--size-caption);
	}

	.counts {
		font-family: var(--face-mono);
		font-size: var(--size-caption);
		font-variant-numeric: tabular-nums;
	}

	.added {
		color: var(--added-text);
		font-weight: var(--weight-strong);
	}

	.removed {
		color: var(--failed-text);
		font-weight: 400;
	}

	.omitted {
		margin: var(--space-3) 0 0;
		color: var(--dim);
		font-size: var(--size-caption);
	}

	.diff {
		max-height: 24rem;
		margin-block-start: var(--space-3);
		overflow: auto;
		overscroll-behavior: contain;
		padding: var(--space-2) var(--space-3);
		border-radius: var(--radius-tag);
		background: var(--surface);
		color: var(--dim);
		font-family: var(--face-mono);
		font-size: var(--size-caption);
		line-height: 1.55;
	}

	.diff ins,
	.diff del {
		text-decoration: none;
	}

	.diff ins {
		color: var(--added-text);
	}

	.diff del {
		color: var(--failed-text);
		font-weight: 400;
	}

	.hunk {
		color: var(--dim);
	}

	.more {
		display: block;
		color: var(--dim);
	}
</style>
