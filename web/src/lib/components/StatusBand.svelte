<script lang="ts">
import {
	approvalGlyphs,
	contextFraction,
	effortGlyph,
	folderGlyph,
	moonGlyph,
	queueGlyph,
} from "#lib/band.ts";
import Mark from "#lib/components/Mark.svelte";
import { countLabel } from "#lib/labels.ts";
import type { ModelOption, StatusInfo, TokenUsage } from "#lib/types.ts";

interface Props {
	status: StatusInfo;
	models: readonly ModelOption[];
	selectedModel: string;
	modelDisabled: boolean;
	usage: TokenUsage | null;
	statusHide: readonly string[];
	folder: string;
	queued: number;
	onOpenModel: () => void;
}

let {
	status,
	models,
	selectedModel,
	modelDisabled,
	usage,
	statusHide,
	folder,
	queued,
	onOpenModel,
}: Props = $props();

const hidden = $derived(new Set(statusHide));
const model = $derived(models.find((entry) => entry.id === status.model));
const levels = $derived(model?.efforts ?? []);
const effort = $derived(status.effort ?? model?.defaultEffort ?? null);
const rank = $derived(effort === null ? -1 : levels.indexOf(effort));
const fraction = $derived(usage === null ? null : contextFraction(usage));
const percent = $derived(fraction === null ? 0 : Math.round(fraction * 100));
const sandboxGlyph = $derived(
	status.sandbox === "off" ? "\u{F002A}" : "\u{F03D7}",
);
const queueLabel = $derived(
	`${countLabel(queued, "follow-up", "follow-ups")} queued`,
);
const shown = $derived({
	effort: levels.length >= 2 && rank >= 0 && !hidden.has("effort"),
	approval: !hidden.has("approval"),
	sandbox: !hidden.has("sandbox"),
	folder: folder !== "" && !hidden.has("folder"),
	queue: queued > 0 && !hidden.has("queue"),
	context: fraction !== null && !hidden.has("context"),
	mark: !hidden.has("mark"),
});
</script>

<div class="status-band">
	<dl class="band">
		<div class="item model" data-item="model" style:--band-row="1">
			<dt class="sr-only">Model</dt>
			<dd>
				<button
					type="button"
					class="model-button"
					disabled={modelDisabled}
					onclick={() => onOpenModel()}
				>
					{selectedModel === "" ? "Choose a model" : selectedModel}
				</button>
			</dd>
		</div>
		{#if shown.effort}
			<div class="item" data-item="effort" style:--band-row="2">
				<dt class="sr-only">Effort</dt>
				<dd>
					<span class="glyph" role="img" aria-label="Effort {effort}"
						>{effortGlyph(rank, levels.length)}</span
					>
				</dd>
			</div>
		{/if}
		{#if shown.approval}
			<div class="item" data-item="approval" style:--band-row="2">
				<dt class="sr-only">Approval</dt>
				<dd>
					<span class="glyph" role="img" aria-label="Approval {status.approval}"
						>{approvalGlyphs[status.approval] ?? status.approval}</span
					>
				</dd>
			</div>
		{/if}
		{#if shown.sandbox}
			<div class="item" data-item="sandbox" style:--band-row="2">
				<dt class="sr-only">Sandbox</dt>
				<dd>
					<span class="glyph" role="img" aria-label="Sandbox {status.sandbox}"
						>{sandboxGlyph}</span
					>
				</dd>
			</div>
		{/if}
		{#if shown.folder}
			<div class="item" data-item="folder" style:--band-row="2">
				<dt class="sr-only">Folder</dt>
				<dd>
					<span class="glyph" role="img" aria-label={folder} title={folder}
						>{folderGlyph}</span
					>
				</dd>
			</div>
		{/if}
		{#if shown.queue}
			<div class="item" data-item="queue" style:--band-row="2">
				<dt class="sr-only">Queue</dt>
				<dd>
					<span class="glyph" role="img" aria-label={queueLabel}>{queueGlyph}</span>
					<span class="count" aria-hidden="true">{queued}</span>
				</dd>
			</div>
		{/if}
		{#if shown.context && fraction !== null}
			<div class="item" data-item="context" style:--band-row="3">
				<dt class="sr-only">Context</dt>
				<dd class="gauge">
					<span class="glyph" role="img" aria-label="Context {percent}% used"
						>{moonGlyph(fraction)}</span
					>
					<span class="percent">{percent}%</span>
				</dd>
			</div>
		{/if}
		{#if shown.mark}
			<div class="item mark-item" data-item="mark" style:--band-row="3">
				<dt class="sr-only">Mark</dt>
				<dd><Mark size={20} /></dd>
			</div>
		{/if}
	</dl>
	<details class="band-more" class:on={statusHide.length > 0}>
		<summary>Status</summary>
		<ul>
			<li>model {status.model}</li>
			{#if shown.effort}<li>effort {effort}</li>{/if}
			{#if shown.approval}<li>approval {status.approval}</li>{/if}
			{#if shown.sandbox}<li>sandbox {status.sandbox}</li>{/if}
			{#if shown.folder}<li>folder {folder}</li>{/if}
			{#if shown.queue}<li>{queueLabel}</li>{/if}
			{#if shown.context}<li>context {percent}% used</li>{/if}
			{#if shown.mark}<li>mark dalgona</li>{/if}
		</ul>
	</details>
</div>

<style>
	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip: rect(0 0 0 0);
		white-space: nowrap;
	}

	.status-band {
		display: grid;
		justify-items: end;
		gap: var(--space-2);
		margin-inline-start: auto;
	}

	.band {
		display: grid;
		grid-auto-flow: column;
		grid-auto-columns: max-content;
		gap: var(--space-1) var(--space-3);
		margin: 0;
		font-size: var(--size-caption);
		font-variant-numeric: tabular-nums;
	}

	.item {
		display: flex;
		align-items: center;
		gap: var(--space-1);
		min-width: 0;
		grid-row: var(--band-row);
	}

	.item dd {
		display: flex;
		align-items: center;
		gap: var(--space-1);
		margin: 0;
		min-width: 0;
	}

	.item :global(.mark) {
		margin: 0;
	}

	.item.model .model-button {
		max-width: 20rem;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.glyph {
		font-family: "Symbols Nerd Subset", sans-serif;
		font-size: var(--size-small);
		line-height: 1;
	}

	.gauge .percent {
		font-weight: var(--weight-strong);
	}

	.band-more {
		display: none;
		font-size: var(--size-caption);
	}

	.band-more.on {
		display: block;
	}

	.band-more ul {
		margin: var(--space-1) 0 0;
		padding-inline-start: var(--space-4);
	}

	@media (max-width: 40rem) {
		.status-band {
			justify-items: stretch;
			margin-inline-start: 0;
			min-width: 0;
			width: 100%;
			max-width: 100%;
		}

		.band {
			container: band / inline-size;
			width: 100%;
			min-width: 0;
			grid-template-columns: minmax(0, 1fr);
		}

		.item.model .model-button {
			max-width: 100%;
		}

		.item {
			grid-row: 1;
		}

		.band-more {
			display: block;
		}
	}

	@container band (max-width: 30rem) {
		[data-item="folder"] {
			display: none;
		}
	}

	@container band (max-width: 26rem) {
		[data-item="queue"] {
			display: none;
		}
	}

	@container band (max-width: 16rem) {
		[data-item="effort"] {
			display: none;
		}
	}

	@container band (max-width: 12rem) {
		[data-item="mark"] {
			display: none;
		}
	}
</style>
