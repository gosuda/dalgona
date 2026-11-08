<script lang="ts">
import type { ModelOption } from "#lib/types.ts";

interface Props {
	models: readonly ModelOption[];
	value: string;
	disabled: boolean;
	onChange: (model: string) => void;
}

let { models, value, disabled, onChange }: Props = $props();
</script>

<div class="model-picker">
	<label for="model">Model</label>
	<select
		id="model"
		name="model"
		value={value}
		disabled={disabled || models.length === 0}
		onchange={(event) => onChange(event.currentTarget.value)}
	>
		{#if models.length === 0}
			<option value="">No models available</option>
		{:else}
			{#each models.filter((model) => !model.hidden) as model (model.id)}
				<option value={model.id} title={model.description}>{model.label}</option>
			{/each}
		{/if}
	</select>
</div>

<style>
	.model-picker {
		display: flex;
		align-items: center;
		gap: var(--space-2);
	}

	label {
		color: var(--color-text-secondary);
		font-size: var(--text-caption);
		font-weight: 600;
	}

	select {
		max-width: 20rem;
		min-width: 0;
		border: 1px solid var(--color-border-strong);
		border-radius: var(--radius-small);
		background: var(--color-bg-surface);
		color: var(--color-text);
		padding: var(--space-2) var(--space-3);
		font-size: var(--text-body-small);
	}
</style>
