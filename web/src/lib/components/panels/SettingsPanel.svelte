<script lang="ts">
import { untrack } from "svelte";
import Panel from "#lib/components/panels/Panel.svelte";
import type { Session } from "#lib/session.svelte.ts";
import {
	approvalOptions,
	draftFromSettings,
	draftProblem,
	hideableItemOptions,
	type SettingsDraft,
	settingEdits,
	themeOptions,
	verbosityOptions,
} from "#lib/settings.ts";

interface Props {
	readonly session: Session;
	readonly open: boolean;
	readonly onClose: () => void;
}

type EditableDraft = { -readonly [K in keyof SettingsDraft]: SettingsDraft[K] };

let { session, open, onClose }: Props = $props();
const groupName = $props.id();
let draft: EditableDraft = $state(draftFromSettings([]));
let problem: string | null = $state(null);
let saving = $state(false);

$effect(() => {
	if (!open) return;
	draft = draftFromSettings(untrack(() => session.settingValues));
	problem = null;
});

async function save(): Promise<void> {
	problem = draftProblem(draft);
	if (problem !== null) return;
	const edits = settingEdits(draftFromSettings(session.settingValues), draft);
	if (edits.length === 0) {
		onClose();
		return;
	}
	saving = true;
	await session.saveSettings(edits);
	saving = false;
}
</script>

<Panel title="/settings" {open} {onClose}>
	{#snippet children()}
		<form
			class="settings"
			onsubmit={(event) => {
				event.preventDefault();
				void save();
			}}
		>
			<fieldset>
				<legend>Approval</legend>
				{#each approvalOptions as option (option.value)}
					<label>
						<input
							type="radio"
							name="{groupName}-approval"
							value={option.value}
							bind:group={draft.approval}
						/>
						{option.label}
					</label>
				{/each}
			</fieldset>
			<fieldset>
				<legend>Verbosity</legend>
				{#each verbosityOptions as option (option.value)}
					<label>
						<input
							type="radio"
							name="{groupName}-verbosity"
							value={option.value}
							bind:group={draft.verbosity}
						/>
						{option.label}
					</label>
				{/each}
			</fieldset>
			<fieldset>
				<legend>Theme</legend>
				{#each themeOptions as option (option.value)}
					<label>
						<input
							type="radio"
							name="{groupName}-theme"
							value={option.value}
							bind:group={draft.theme}
						/>
						{option.label}
					</label>
				{/each}
			</fieldset>
			<fieldset>
				<legend>Hidden status items</legend>
				{#each hideableItemOptions as option (option.value)}
					<label>
						<input
							type="checkbox"
							value={option.value}
							bind:group={draft.statusHide}
						/>
						{option.label}
					</label>
				{/each}
			</fieldset>
			<label class="single">
				<input type="checkbox" bind:checked={draft.editReview} />
				Review edits
			</label>
			<p class="row">Desktop notifications</p>
			{#if problem !== null}
				<p class="problem" role="alert">{problem}</p>
			{/if}
			<div class="actions">
				<button type="button" class="quiet" onclick={() => onClose()}>
					Cancel
				</button>
				<button type="submit" class="point" disabled={saving}>Save</button>
			</div>
		</form>
	{/snippet}
</Panel>

<style>
	.settings {
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
		min-height: var(--control-height);
	}

	.single {
		justify-content: flex-start;
	}

	.row {
		margin: 0;
		color: var(--dim);
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
