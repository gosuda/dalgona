<script lang="ts">
import StateTag from "#lib/components/StateTag.svelte";
import { threadHue } from "#lib/hue.ts";
import { type StateKey, stateKey, threadTitle } from "#lib/labels.ts";
import type { ThreadSummary } from "#lib/types.ts";

interface Props {
	threads: readonly ThreadSummary[];
	activeId: string | null;
	loading: boolean;
	connected: boolean;
	disabled: boolean;
	starting: boolean;
	waitingThreadIds: ReadonlySet<string>;
	completedThreadIds: ReadonlySet<string>;
	onNew: () => void;
	onSelect: (threadId: string) => void;
}

let {
	threads,
	activeId,
	loading,
	connected,
	disabled,
	starting,
	waitingThreadIds,
	completedThreadIds,
	onNew,
	onSelect,
}: Props = $props();

const dateFormatter = new Intl.DateTimeFormat(undefined, {
	month: "short",
	day: "numeric",
});

function rowKey(thread: ThreadSummary): StateKey | null {
	if (waitingThreadIds.has(thread.id)) {
		return "awaitingApproval";
	}
	const key = stateKey(thread.status, thread.activeFlags);
	return key === "idle" && completedThreadIds.has(thread.id)
		? "completed"
		: key;
}
</script>

<aside class="sidebar" aria-label="Thread navigation">
	<header class="brand">
		<div>
			<p class="eyebrow">Agent workbench</p>
			<h1>dalgona<span aria-hidden="true">/</span></h1>
		</div>
		<button
			class="point"
			aria-disabled={starting ? "true" : undefined}
			onclick={() => {
				if (!starting && connected) onNew();
			}}
		>Start thread</button>
	</header>

	<div class="thread-heading">
		<h2>Threads</h2>
		<span class="count">{threads.length}</span>
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
		<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
		<nav aria-label="Conversation threads" tabindex="0">
			<ul>
				{#each threads as thread (thread.id)}
					{@const key = rowKey(thread)}
					<li>
						<button
							class="thread"
							aria-current={thread.id === activeId ? "true" : undefined}
							disabled={disabled || starting}
							onclick={() => onSelect(thread.id)}
						>
							<span class="thread-topline">
								<span class="name">
									<span
										class="hue-dot"
										data-hue={threadHue(thread.id)}
										aria-hidden="true"
									></span>
									<strong class="title">{threadTitle(thread)}</strong>
								</span>
							</span>
							{#if thread.preview && thread.preview !== threadTitle(thread)}
								<span class="preview prose">{thread.preview}</span>
							{/if}
							<span class="meta">
								<span class="state-line">
									{#if key === null}
										<span>{thread.status}</span>
									{:else}
										<StateTag state={key} />
									{/if}
									<time datetime={new Date(thread.updatedAt * 1000).toISOString()}>
										{dateFormatter.format(new Date(thread.updatedAt * 1000))}
									</time>
								</span>
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
		padding: var(--space-6) var(--space-4);
		background: var(--tone);
		border-inline-end: 1px solid var(--line);
	}

	.brand {
		display: flex;
		align-items: end;
		justify-content: space-between;
		gap: var(--space-3);
		margin-block-end: var(--space-8);
	}

	.eyebrow {
		margin: 0 0 var(--space-1);
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 600;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	h1 {
		margin: 0;
		font-size: var(--size-title);
		font-weight: 650;
		letter-spacing: -0.02em;
		line-height: 1.1;
	}

	h1 span {
		color: var(--accent-text);
		margin-inline-start: var(--space-1);
	}

	.thread-heading {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		border-block-end: 1px solid var(--line);
		padding-block-end: var(--space-2);
	}

	.thread-heading h2 {
		margin: 0;
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 600;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	.thread-heading span {
		color: var(--dim);
		font-size: var(--size-caption);
		font-variant-numeric: tabular-nums;
	}

	.loading {
		min-height: 1.5em;
		margin: var(--space-2) 0;
		color: var(--dim);
		font-size: var(--size-caption);
	}

	ul {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: var(--space-2);
		list-style: none;
		margin: 0;
		padding: 0;
	}

	.thread {
		display: grid;
		width: 100%;
		min-width: 0;
		min-height: 0;
		gap: var(--space-2);
		border-color: transparent;
		border-inline-start: 0 solid var(--text);
		background: transparent;
		padding: var(--space-3);
		padding-inline-start: calc(var(--space-3) + 2px);
		text-align: start;
	}

	.thread:disabled {
		opacity: 0.55;
	}

	.thread[aria-current="true"] {
		border-inline-start-width: 2px;
		padding-inline-start: var(--space-3);
		background: var(--tone);
	}

	.thread[aria-current="true"] .title {
		font-weight: var(--weight-strong);
	}

	@media (hover: hover) {
		.thread[aria-current="true"]:not(:disabled):hover {
			border-color: transparent;
			border-inline-start-color: var(--text);
			background: var(--tone);
		}
	}

	@media (forced-colors: active) {
		.thread[aria-current="true"] {
			border-inline-start-color: Highlight;
		}
	}

	.thread-topline {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		min-width: 0;
		gap: var(--space-2);
	}

	.name {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		min-width: 0;
	}

	.hue-dot {
		flex: none;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--point);
	}

	.thread-topline strong {
		min-width: 0;
		overflow-wrap: anywhere;
		font-size: var(--size-small);
		font-weight: 650;
		line-height: 1.4;
	}

	time,
	.meta,
	.preview {
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: 400;
		min-width: 0;
		overflow-wrap: anywhere;
	}

	time {
		flex: none;
		font-variant-numeric: tabular-nums;
	}

	.state-line {
		display: flex;
		align-items: baseline;
		gap: var(--space-2);
		min-width: 0;
	}

	.preview {
		line-height: 1.45;
	}

	.meta {
		display: flex;
		flex-wrap: wrap;
		justify-content: space-between;
		gap: var(--space-2);
	}

	.empty {
		padding-block: var(--space-6);
		color: var(--dim);
		font-size: var(--size-small);
	}

	.empty p {
		margin: 0;
	}

	.empty p:first-child {
		color: var(--text);
		font-weight: 650;
	}

	.empty p + p {
		margin-block-start: var(--space-1);
	}

	@media (max-width: 52rem) {
		.sidebar {
			border-inline-end: 0;
			border-block-end: 1px solid var(--line);
			padding: var(--space-4);
		}

		nav {
			max-height: 18rem;
			overflow-y: auto;
			overscroll-behavior: contain;
		}

		.brand {
			margin-block-end: var(--space-4);
		}
	}
</style>
