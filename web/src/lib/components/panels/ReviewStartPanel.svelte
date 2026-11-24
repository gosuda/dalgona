<script lang="ts">
import Panel from "#lib/components/panels/Panel.svelte";
import {
	faultNotices,
	isWhitespaceOnly,
	multilineText,
	refuseSingleLineInsertion,
	selectDeletedCluster,
	singleLineFault,
	type TextFault,
	textFault,
} from "#lib/inputtext.ts";
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
let busy = $state(false);

function refusal(
	fault: TextFault | null,
	value: string,
	blankNotice: string,
): string | null {
	if (fault !== null) return faultNotices[fault];
	return isWhitespaceOnly(value) ? blankNotice : null;
}

function target(): ReviewTarget | string {
	if (kind === "baseBranch") {
		const problem = refusal(
			singleLineFault(branch),
			branch,
			"Enter a base branch.",
		);
		return problem ?? { type: kind, branch };
	}
	if (kind === "commit") {
		const problem = refusal(singleLineFault(sha), sha, "Enter a commit id.");
		return problem ?? { type: kind, sha };
	}
	if (kind === "custom") {
		const problem = refusal(
			textFault(instructions),
			instructions,
			"Enter review instructions.",
		);
		return problem ?? { type: kind, instructions };
	}
	return { type: kind };
}

function guardLine(event: ClipboardEvent | DragEvent): void {
	const fault = refuseSingleLineInsertion(event);
	error = fault === null ? "" : faultNotices[fault];
}

function typeInstructions(event: Event): void {
	if (event instanceof InputEvent && event.isComposing) return;
	instructions = multilineText(instructions);
}

async function start(): Promise<void> {
	if (busy) return;
	error = "";
	const choice = target();
	if (typeof choice === "string") {
		error = choice;
		return;
	}
	const opened = session.openedPanel;
	busy = true;
	try {
		await session.startReview(choice);
		session.closePanelIf(opened);
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	} finally {
		busy = false;
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
				<input
					type="text"
					bind:value={branch}
					aria-label="Base branch name"
					aria-invalid={error === "" ? "false" : "true"}
					aria-describedby={error === "" ? undefined : "review-error"}
					onpaste={guardLine}
					ondrop={guardLine}
					onkeydown={(event) => selectDeletedCluster(event, event.currentTarget)}
				/>
			{/if}
			<label><input type="radio" name="review-target" value="commit" bind:group={kind} /> Commit</label>
			{#if kind === "commit"}
				<input
					type="text"
					bind:value={sha}
					aria-label="Commit id"
					aria-invalid={error === "" ? "false" : "true"}
					aria-describedby={error === "" ? undefined : "review-error"}
					onpaste={guardLine}
					ondrop={guardLine}
					onkeydown={(event) => selectDeletedCluster(event, event.currentTarget)}
				/>
			{/if}
			<label><input type="radio" name="review-target" value="custom" bind:group={kind} /> Custom</label>
			{#if kind === "custom"}
				<textarea
					bind:value={instructions}
					aria-label="Review instructions"
					aria-invalid={error === "" ? "false" : "true"}
					aria-describedby={error === "" ? undefined : "review-error"}
					oninput={typeInstructions}
					onkeydown={(event) => selectDeletedCluster(event, event.currentTarget)}
				></textarea>
			{/if}
		</fieldset>
		{#if error !== ""}
			<p class="error" id="review-error" role="alert">{error}</p>
		{/if}
	{/snippet}
	{#snippet footer()}
		<button type="button" class="point" aria-disabled={busy ? "true" : undefined} onclick={() => void start()}>Start review</button>
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
