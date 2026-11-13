<script lang="ts">
import Panel from "#lib/components/panels/Panel.svelte";
import { advisorLabel } from "#lib/labels.ts";
import type { Session } from "#lib/session.svelte.ts";

interface Props {
	readonly session: Session;
	readonly open: boolean;
}

let { session, open }: Props = $props();
let error = $state("");
const advisors = $derived(
	session.advisor.kind === "loaded" ? session.advisor.advisors : [],
);
const enabled = $derived(
	advisors.some((advisor) => advisor.status !== "disabled"),
);

$effect(() => {
	if (open) void refresh();
});

async function refresh(): Promise<void> {
	error = "";
	try {
		await session.refreshAdvisor();
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	}
}

async function run(action: () => Promise<void>): Promise<void> {
	error = "";
	try {
		await action();
	} catch (failure) {
		error = failure instanceof Error ? failure.message : String(failure);
	}
}
</script>

<Panel title="Advisor" {open} onClose={() => session.closePanel()}>
	{#snippet children()}
		{#if session.advisor.kind === "unknown"}
			<p class="empty">Reading advisor status.</p>
		{:else if session.advisor.kind === "unavailable"}
			<p class="empty">The advisor status could not be read.</p>
		{:else if advisors.length === 0}
			<p class="empty">No advisor is attached to this thread.</p>
		{:else}
			<ul class="advisors">
				{#each advisors as advisor (advisor.name)}
					<li>
						<span>{advisor.name} · {advisor.model}</span>
						<span class="status">{advisorLabel({ kind: "loaded", advisors: [advisor] })}</span>
					</li>
				{/each}
			</ul>
		{/if}
		{#if error !== ""}
			<p class="error" role="alert">{error}</p>
		{/if}
	{/snippet}
	{#snippet footer()}
		<button type="button" class="point" onclick={() => void run(() => session.setAdvisor(!enabled))}>
			{enabled ? "Turn advisor off" : "Turn advisor on"}
		</button>
		<button type="button" class="quiet" onclick={() => void run(() => session.resetAdvisor())}>Reset advisor</button>
	{/snippet}
</Panel>

<style>
	.advisors {
		display: grid;
		gap: var(--space-2);
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.advisors li {
		display: flex;
		justify-content: space-between;
		gap: var(--space-3);
	}

	.status,
	.empty {
		color: var(--dim);
	}

	.error {
		color: var(--failed-text);
		font-weight: var(--weight-strong);
	}
</style>
