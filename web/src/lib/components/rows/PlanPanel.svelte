<script lang="ts">
import { planStatusLabel } from "#lib/labels.ts";
import type { TurnPlan } from "#lib/types.ts";

interface Props {
	readonly plan: TurnPlan | null;
}

let { plan }: Props = $props();
</script>

{#if plan}
	<section class="turn-plan" aria-label="Plan">
		<span class="item-label">Plan</span>
		{#if plan.explanation}
			<p>{plan.explanation}</p>
		{/if}
		<ol>
			{#each plan.steps as step, index (index)}
				<li data-status={step.status} aria-current={step.status === "inProgress" ? "step" : undefined}>
					<span class="plan-status">{planStatusLabel(step.status)}</span>
					<span>{step.step}</span>
				</li>
			{/each}
		</ol>
	</section>
{/if}

<style>
	.item-label {
		display: block;
		margin-block-end: var(--space-2);
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 650;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	.turn-plan {
		max-width: 70ch;
		margin-block-start: var(--space-6);
		font-size: var(--size-small);
	}

	.turn-plan p {
		margin: 0 0 var(--space-2);
		color: var(--dim);
	}

	.turn-plan ol {
		display: grid;
		gap: var(--space-1);
		list-style: none;
		margin: 0;
		padding: 0;
	}

	.turn-plan li {
		display: grid;
		grid-template-columns: 6rem minmax(0, 1fr);
		gap: var(--space-2);
		overflow-wrap: break-word;
	}

	.plan-status {
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 600;
	}

	.turn-plan li[data-status="inProgress"] {
		font-weight: 650;
	}

	.turn-plan li[data-status="completed"] > span:last-child {
		color: var(--dim);
	}
</style>
