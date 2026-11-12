<script lang="ts">
import type { Chip } from "#lib/attachments.ts";
import { handoffFocus } from "#lib/focus.ts";

interface Props {
	chips: readonly Chip[];
	onRemove: (id: number) => void;
}

let { chips, onRemove }: Props = $props();
let shown = $state<Readonly<Record<number, boolean>>>({});

function sizeLabel(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function toggle(id: number): void {
	shown = { ...shown, [id]: !shown[id] };
}

function promptField(): HTMLElement | undefined {
	return document.getElementById("prompt") ?? undefined;
}
</script>

{#if chips.length > 0}
	<ul class="chips" aria-label="Attachments">
		{#each chips as chip (chip.id)}
			<li class="chip">
				{#if chip.kind === "paste"}
					<span class="label">Pasted text · {chip.lines} lines</span>
					<button
						type="button"
						aria-expanded={shown[chip.id] ? "true" : "false"}
						onclick={() => toggle(chip.id)}
					>{shown[chip.id] ? "Hide" : "Show"}</button>
				{:else}
					<span class="label">Image · {chip.name} · {sizeLabel(chip.size)}</span>
				{/if}
				<button type="button" {@attach handoffFocus(promptField)} onclick={() => onRemove(chip.id)}>Remove</button>
				{#if chip.kind === "paste" && shown[chip.id]}
					<pre>{chip.text}</pre>
				{/if}
			</li>
		{/each}
	</ul>
{/if}

<style>
	.chips {
		list-style: none;
		margin: var(--space-2) 0 0;
		padding: 0;
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.chip {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2);
		max-width: 100%;
		min-width: 0;
		border: var(--border-width) solid var(--line);
		border-radius: var(--radius-tag);
		padding: var(--space-1) var(--space-2);
		font-size: var(--size-caption);
	}

	.label {
		min-width: 0;
		overflow-wrap: anywhere;
	}

	pre {
		flex-basis: 100%;
		margin: 0;
		max-height: 12rem;
		overflow: auto;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		font-family: var(--face-mono);
	}
</style>
