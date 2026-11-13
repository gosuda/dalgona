<script lang="ts">
import { untrack } from "svelte";
import Panel from "#lib/components/panels/Panel.svelte";
import type { Session } from "#lib/session.svelte.ts";
import type { ModelOption, ModelPreset } from "#lib/types.ts";

interface Props {
	readonly session: Session;
	readonly open: boolean;
	readonly onClose: () => void;
}

let { session, open, onClose }: Props = $props();
const groupName = $props.id();
const presetOrder: readonly ModelPreset[] = ["default", "smol", "slow"];
let model = $state("");
let effort = $state("");
let preset = $state("");
let problem: string | null = $state(null);
let saving = $state(false);

const visible = $derived(session.models.filter((entry) => !entry.hidden));
const presetRows = $derived(
	presetOrder.flatMap((name) =>
		visible
			.filter((entry) => entry.presets.includes(name))
			.map((entry) => ({ name, entry, key: `${name}:${entry.id}` })),
	),
);
const chosen = $derived(visible.find((entry) => entry.id === model));
const levels = $derived(chosen?.efforts ?? []);

$effect(() => {
	if (!open) return;
	untrack(() => {
		const current = session.models.find(
			(entry) => entry.id === session.selectedModel,
		);
		model = session.selectedModel;
		effort = session.status.effort ?? current?.defaultEffort ?? "";
		preset = "";
		problem = null;
	});
});

function presetText(name: ModelPreset, entry: ModelOption): string {
	return [name, entry.label, entry.defaultEffort]
		.filter((part): part is string => part !== null && part !== "")
		.join(" · ");
}

function fallbackEffort(entry: ModelOption): string {
	return entry.defaultEffort ?? entry.efforts[0] ?? "";
}

function pickPreset(entry: ModelOption, key: string): void {
	model = entry.id;
	effort = fallbackEffort(entry);
	preset = key;
	problem = null;
}

function pickModel(entry: ModelOption): void {
	model = entry.id;
	if (!entry.efforts.includes(effort)) effort = fallbackEffort(entry);
	preset = "";
	problem = null;
}

function modelProblem(): string | null {
	if (chosen === undefined) return "Choose a model.";
	if (levels.length > 0 && !levels.includes(effort))
		return `Choose an effort for ${model}.`;
	return null;
}

async function save(): Promise<void> {
	problem = modelProblem();
	if (problem !== null) return;
	saving = true;
	await session.saveModel(model, levels.length > 0 ? effort : null);
	saving = false;
}
</script>

<Panel title="/model" {open} {onClose}>
	{#snippet children()}
		{#if visible.length === 0}
			<p>No models. Log in to a provider to list its models.</p>
			<div class="actions">
				<button type="button" class="point" onclick={() => session.openPanel("login")}>Log in</button>
			</div>
		{:else}
			<form
				class="model"
				onsubmit={(event) => {
					event.preventDefault();
					void save();
				}}
			>
				{#if presetRows.length > 0}
					<fieldset>
						<legend>Presets</legend>
						{#each presetRows as row (row.key)}
							<label>
								<input
									type="radio"
									name="{groupName}-preset"
									value={row.key}
									checked={preset === row.key}
									onchange={() => pickPreset(row.entry, row.key)}
								/>
								{presetText(row.name, row.entry)}
							</label>
						{/each}
					</fieldset>
				{/if}
				<fieldset>
					<legend>Models</legend>
					{#each visible as entry (entry.id)}
						<div class="option">
							<label>
								<input
									type="radio"
									name="{groupName}-model"
									value={entry.id}
									checked={model === entry.id}
									onchange={() => pickModel(entry)}
								/>
								{entry.label}
							</label>
							{#if entry.description !== ""}
								<p class="note">{entry.description}</p>
							{/if}
						</div>
					{/each}
				</fieldset>
				{#if levels.length > 0}
					<fieldset>
						<legend>Effort</legend>
						{#each levels as level (level)}
							<label>
								<input
									type="radio"
									name="{groupName}-effort"
									value={level}
									checked={effort === level}
									onchange={() => (effort = level)}
								/>
								{level}
							</label>
						{/each}
					</fieldset>
				{/if}
				{#if problem !== null}
					<p class="problem" role="alert">{problem}</p>
				{/if}
				<div class="actions">
					<button type="button" class="quiet" onclick={() => onClose()}>Cancel</button>
					<button type="submit" class="point" disabled={saving}>Use model</button>
				</div>
			</form>
		{/if}
	{/snippet}
</Panel>

<style>
	.model {
		display: grid;
		gap: var(--space-3);
	}

	fieldset {
		display: grid;
		gap: var(--space-2);
		border: var(--border-width) solid var(--line);
		border-radius: var(--radius-card);
		padding: var(--space-2) var(--space-3);
	}

	legend {
		font-weight: var(--weight-title);
	}

	label {
		display: flex;
		align-items: center;
		gap: var(--space-2);
	}

	.option {
		display: grid;
		gap: var(--space-1);
	}

	.note {
		margin: 0;
		color: var(--dim);
		font-size: var(--size-small);
	}

	.problem {
		margin: 0;
		color: var(--failed-text);
		font-weight: var(--weight-strong);
	}

	.actions {
		display: flex;
		justify-content: flex-end;
		gap: var(--space-3);
	}
</style>
