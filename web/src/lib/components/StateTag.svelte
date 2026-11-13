<script lang="ts">
import { untrack } from "svelte";
import { MediaQuery } from "svelte/reactivity";
import { formatDuration, formatRetryWait } from "#lib/format.ts";
import { type StateKey, stateWords } from "#lib/labels.ts";

interface Props {
	state: StateKey;
	startedAtMs?: number | null;
	tick?: number;
	point?: boolean;
	retryRemainingMs?: number | null;
}

let {
	state: key,
	startedAtMs = null,
	tick = 0,
	point = false,
	retryRemainingMs = null,
}: Props = $props();

const reducedMotion = new MediaQuery("(prefers-reduced-motion: reduce)");

let clock = $state(Date.now());

$effect(() => {
	void tick;
	if (key !== "inProgress" || !untrack(() => reducedMotion.current)) return;
	clock = Date.now();
});

$effect(() => {
	if (key !== "inProgress" || reducedMotion.current) return;
	clock = Date.now();
	const timer = setInterval(() => {
		clock = Date.now();
	}, 1000);
	return () => clearInterval(timer);
});

let elapsed = $derived(
	startedAtMs === null ? "" : formatDuration(Math.max(0, clock - startedAtMs)),
);

let word = $derived(stateWords[key]);
let retry = $derived(
	retryRemainingMs === null ? "" : formatRetryWait(retryRemainingMs),
);
</script>

{#if key === "inProgress"}
	<span class="tag live" class:point data-tick={tick}>
		<span class="dot" aria-hidden="true"></span>
		<span class="word">{word}</span>
		<span class="elapsed">{retry || elapsed}</span>
	</span>
{:else if key === "failed"}
	<span class="failed">{word}</span>
{:else}
	<span class="tag">{word}</span>
{/if}

<style>
	.tag {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
		font-size: var(--size-small);
		font-variant-numeric: tabular-nums;
	}

	.tag.point {
		border-radius: var(--radius-tag);
		padding: 0 var(--space-2);
		border: 0;
		background: var(--point);
		color: var(--on-point);
	}
	@media (forced-colors: active) {
		.tag.point {
			border: 1px solid CanvasText;
		}
	}

	.elapsed {
		display: inline-block;
		min-width: 7ch;
		text-align: end;
	}

	.dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: currentColor;
	}

	@media (prefers-reduced-motion: no-preference) {
		.tag.live .dot {
			animation: breath var(--motion-breath) ease-in-out infinite;
		}
	}

	@keyframes breath {
		50% {
			opacity: 0.4;
		}
	}

	.failed {
		color: var(--failed-text);
		font-weight: var(--weight-strong);
		font-size: var(--size-small);
	}
</style>
