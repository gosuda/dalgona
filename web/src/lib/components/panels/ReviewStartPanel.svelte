<script lang="ts">
import Panel from "#lib/components/panels/Panel.svelte";
import type { Session } from "#lib/session.svelte.ts";
import type { ReviewTarget } from "#lib/types.ts";

interface Props {
	readonly session: Session;
	readonly open: boolean;
}

let { session, open }: Props = $props();
let kind = $state<ReviewTarget["type"]>("uncommittedChanges");
let branch = $state("");
let sha = $state("");
let instructions = $state("");
let error = $state("");

function target(): ReviewTarget | string {
	if (kind === "baseBranch") {
		const name = branch.trim();
		return name === "" ? "Enter a base branch." : { type: kind, branch: name };
	}
	if (kind === "commit") {
		const id = sha.trim();
		return id === "" ? "Enter a commit id." : { type: kind, sha: id };
	}
	if (kind === "custom") {
		const text = instructions.trim();
		return text === ""
			? "Enter review instructions."
			: { type: kind, instructions: text };
	}
	return { type: kind };
}

async function start(): Promise<void> {
	error = "";
	const choice = target();
	if (typeof choice === "string") {
		error = choice;
		return;
	}
	try {
		await session.startReview(choice);
		session.closePanel();
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	}
}
</script>

<Panel title="Review changes" {open} onClose={() => session.closePanel()}>
	{#snippet children()}
		<fieldset>
			<legend>Review target</legend>
			<label><input type="radio" name="review-target" value="uncommittedChanges" bind:group={kind} /> Uncommitted changes</label>
			<label><input type="radio" name="review-target" value="baseBranch" bind:group={kind} /> Base branch</label>
			{#if kind === "baseBranch"}
				<input type="text" bind:value={branch} aria-label="Base branch name" />
			{/if}
			<label><input type="radio" name="review-target" value="commit" bind:group={kind} /> Commit</label>
			{#if kind === "commit"}
				<input type="text" bind:value={sha} aria-label="Commit id" />
			{/if}
			<label><input type="radio" name="review-target" value="custom" bind:group={kind} /> Custom</label>
			{#if kind === "custom"}
				<textarea bind:value={instructions} aria-label="Review instructions"></textarea>
			{/if}
		</fieldset>
		{#if error !== ""}
			<p class="error" role="alert">{error}</p>
		{/if}
	{/snippet}
	{#snippet footer()}
		<button type="button" class="point" onclick={() => void start()}>Start review</button>
		<button type="button" class="quiet" onclick={() => session.closePanel()}>Cancel</button>
	{/snippet}
</Panel>

<style>
	fieldset {
		display: grid;
		gap: var(--space-2);
		margin: 0;
		border: var(--border-width) solid var(--line);
		border-radius: var(--radius-control);
		padding: var(--space-3);
	}

	legend {
		padding-inline: var(--space-2);
		color: var(--dim);
	}

	.error {
		color: var(--failed-text);
		font-weight: var(--weight-strong);
	}
</style>
