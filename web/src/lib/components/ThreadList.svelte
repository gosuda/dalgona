<script lang="ts">
import { threadTitle } from "#lib/labels.ts";
import type { ThreadSummary } from "#lib/types.ts";

interface Props {
	threads: readonly ThreadSummary[];
	activeId: string | null;
	loading: boolean;
	connected: boolean;
	disabled: boolean;
	onNew: () => void;
	onSelect: (threadId: string) => void;
}

let {
	threads,
	activeId,
	loading,
	connected,
	disabled,
	onNew,
	onSelect,
}: Props = $props();

const dateFormatter = new Intl.DateTimeFormat(undefined, {
	month: "short",
	day: "numeric",
});

function activityLabel(thread: ThreadSummary): string {
	return thread.status === "active" ? "Working" : "Idle";
}
</script>

<aside class="sidebar" aria-label="Thread navigation">
	<header class="brand">
		<div>
			<p class="eyebrow">Agent workbench</p>
			<h1>dalgona<span aria-hidden="true">/</span></h1>
		</div>
		<button class="primary" onclick={onNew} disabled={disabled}>New thread</button>
	</header>

	<div class="thread-heading">
		<h2>Threads</h2>
		<span>{threads.length}</span>
	</div>

	<p class="loading" role="status">{loading ? "Loading threads…" : ""}</p>

	{#if !loading && threads.length === 0 && !connected}
		<div class="empty">
			<p>Threads unavailable</p>
			<p>Connect to dalgona to see your threads.</p>
		</div>
	{:else if !loading && threads.length === 0}
		<div class="empty">
			<p>No threads yet</p>
			<p>Start a thread to give the agent a task.</p>
		</div>
	{:else}
		<nav aria-label="Conversation threads">
			<ul>
				{#each threads as thread (thread.id)}
					<li>
						<button
							class="thread"
							class:selected={thread.id === activeId}
							aria-current={thread.id === activeId ? "page" : undefined}
							onclick={() => onSelect(thread.id)}
						>
							<span class="thread-topline">
								<strong>{threadTitle(thread)}</strong>
								<time datetime={new Date(thread.updatedAt * 1000).toISOString()}>
									{dateFormatter.format(new Date(thread.updatedAt * 1000))}
								</time>
							</span>
							{#if thread.preview && thread.preview !== threadTitle(thread)}
								<span class="preview">{thread.preview}</span>
							{/if}
							<span class="meta">
								<span class:working={thread.status === "active"}>{activityLabel(thread)}</span>
								<span>{thread.model ?? "Default model"}</span>
							</span>
						</button>
					</li>
				{/each}
			</ul>
		</nav>
	{/if}
</aside>

<style>
	.sidebar {
		min-width: 0;
		min-height: 0;
		overflow-y: auto;
		overscroll-behavior: contain;
		padding: var(--space-5) var(--space-4);
		background: var(--color-bg-sidebar);
		border-inline-end: 1px solid var(--color-border);
	}

	.brand {
		display: flex;
		align-items: end;
		justify-content: space-between;
		gap: var(--space-3);
		margin-block-end: var(--space-6);
	}

	.eyebrow {
		margin: 0 0 var(--space-1);
		color: var(--color-text-secondary);
		font-size: var(--text-caption);
		font-weight: 600;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	h1 {
		margin: 0;
		font-size: var(--text-title);
		font-weight: 650;
		letter-spacing: -0.02em;
		line-height: 1.1;
	}

	h1 span {
		color: var(--color-accent);
		margin-inline-start: var(--space-1);
	}

	.thread-heading {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		border-block-end: 1px solid var(--color-border);
		padding-block-end: var(--space-2);
	}

	.thread-heading h2 {
		margin: 0;
		font-size: var(--text-body-small);
		font-weight: 650;
	}

	.thread-heading span {
		color: var(--color-text-secondary);
		font-size: var(--text-caption);
		font-variant-numeric: tabular-nums;
	}

	.loading {
		min-height: 1.5em;
		margin: var(--space-2) 0;
		color: var(--color-text-secondary);
		font-size: var(--text-caption);
	}

	ul {
		display: grid;
		gap: var(--space-2);
		list-style: none;
		margin: 0;
		padding: 0;
	}

	.thread {
		display: grid;
		width: 100%;
		min-height: 0;
		gap: var(--space-2);
		border-color: transparent;
		background: transparent;
		padding: var(--space-3);
		text-align: start;
	}

	.thread.selected {
		border-color: var(--color-accent);
		background: var(--color-bg-selected);
	}

	.thread-topline {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--space-2);
	}

	.thread-topline strong {
		min-width: 0;
		overflow-wrap: break-word;
		font-size: var(--text-body-small);
		font-weight: 650;
		line-height: 1.4;
	}

	time,
	.meta,
	.preview {
		color: var(--color-text-secondary);
		font-size: var(--text-caption);
		font-weight: 400;
	}

	time {
		flex: none;
		font-variant-numeric: tabular-nums;
	}

	.preview {
		line-height: 1.45;
		overflow-wrap: break-word;
	}

	.meta {
		display: flex;
		flex-wrap: wrap;
		justify-content: space-between;
		gap: var(--space-2);
	}

	.meta .working {
		color: var(--color-success);
		font-weight: 650;
	}

	.empty {
		padding-block: var(--space-5);
		color: var(--color-text-secondary);
		font-size: var(--text-body-small);
	}

	.empty p {
		margin: 0;
	}

	.empty p:first-child {
		color: var(--color-text);
		font-weight: 650;
	}

	.empty p + p {
		margin-block-start: var(--space-1);
	}

	@media (max-width: 52rem) {
		.sidebar {
			border-inline-end: 0;
			border-block-end: 1px solid var(--color-border);
			padding: var(--space-4);
		}

		.brand {
			margin-block-end: var(--space-4);
		}
	}
</style>
