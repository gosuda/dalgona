<script lang="ts">
import KeywordText from "#lib/components/KeywordText.svelte";
import Markdown from "#lib/components/Markdown.svelte";
import type { RowProps } from "#lib/components/rows/types.ts";

let { item, running }: RowProps = $props();
</script>

{#if item.kind === "userMessage"}
	<article class="user-message">
		<span class="item-label">You</span>
		<p class="prose"><KeywordText text={item.text} matches={item.keywords} /></p>
		{#if item.keywords.length > 0}
			<p class="keywords">Keywords: {item.keywords.map((match) => match.word).join(", ")}</p>
		{/if}
	</article>
{:else if item.kind === "agentMessage"}
	<article class="agent-message">
		<span class="item-label">dalgona</span>
		<div class="prose">
			<Markdown text={item.text} streaming={!item.complete && running} />
		</div>
	</article>
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

	.user-message {
		border-inline-start: 2px solid var(--accent-text);
		padding: var(--space-2) var(--space-4);
		background: var(--tone);
	}

	.user-message .prose {
		max-width: 70ch;
		margin: 0;
		line-height: 1.6;
		overflow-wrap: break-word;
		white-space: pre-wrap;
		text-wrap: pretty;
	}

	.agent-message {
		padding-block: var(--space-2);
	}

	.agent-message .prose {
		max-width: 70ch;
		line-height: 1.6;
		overflow-wrap: break-word;
		text-wrap: pretty;
	}

	.keywords {
		margin: var(--space-2) 0 0;
		color: var(--dim);
		font-size: var(--size-caption);
	}
</style>
