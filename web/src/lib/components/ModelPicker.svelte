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
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 600;
	}

	select {
		max-width: 20rem;
		min-width: 0;
		border: 1px solid var(--strong);
		border-radius: var(--radius-control);
		background: var(--surface);
		color: var(--text);
		padding: var(--space-2) var(--space-3);
	}
</style>
