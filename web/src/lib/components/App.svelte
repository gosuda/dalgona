<script lang="ts">
import ApprovalDialog from "#lib/components/ApprovalDialog.svelte";
import Composer from "#lib/components/Composer.svelte";
import ModelPicker from "#lib/components/ModelPicker.svelte";
import StatusBar from "#lib/components/StatusBar.svelte";
import ThreadList from "#lib/components/ThreadList.svelte";
import Transcript from "#lib/components/Transcript.svelte";
import type { Session } from "#lib/session.svelte.ts";
import type { ApprovalRequest } from "#lib/types.ts";

interface Props {
	session: Session;
}

let { session }: Props = $props();
</script>

<svelte:head>
	<title>{session.activeTitle} · dalgona</title>
</svelte:head>

<a class="skip-link" href="#main-content">Skip to transcript</a>

<div class="shell">
	<ThreadList
		threads={session.threads}
		activeId={session.activeThreadId}
		loading={session.loadingThreads}
		connected={session.connection === "connected"}
		disabled={session.connection !== "connected" || session.startingThread}
		onNew={() => void session.createThread()}
		onSelect={(threadId) => void session.selectThread(threadId)}
	/>

	<main id="main-content" class="workspace" tabindex="-1">
		<StatusBar
			status={session.status}
			connection={session.connection}
			advisor={session.advisor}
			turnStatus={session.transcript.turnStatus}
			onReconnect={() => void session.connect()}
		/>

		<div class="workspace-heading">
			<div>
				<p>Current thread</p>
				<h2>{session.activeTitle}</h2>
			</div>
			<ModelPicker
				models={session.models}
				value={session.selectedModel}
				disabled={session.connection !== "connected"}
				onChange={(model) => void session.changeModel(model)}
			/>
		</div>

		{#if session.errorMessage}
			<div class="error-banner" role="alert">
				<p>{session.errorMessage}</p>
				<button class="quiet" onclick={() => session.clearError()}>Dismiss error</button>
			</div>
		{/if}

		<Transcript
			threadId={session.activeThreadId}
			items={session.transcript.items}
			turnStatus={session.transcript.turnStatus}
			error={session.transcript.error}
			plan={session.transcript.plan}
			connected={session.connection === "connected"}
			keywords={session.keywords}
			loading={session.loadingTranscript}
			awaitingApproval={session.awaitingApproval}
		/>

		<Composer
			disabled={session.connection !== "connected" || session.activeThreadId === null || session.loadingTranscript}
			running={session.running}
			queuedCount={session.queuedFollowups.length}
			keywords={session.keywords}
			onSend={(text) => void session.sendMessage(text)}
			onSteer={(text) => void session.steerMessage(text)}
			onFollowup={(text) => session.queueFollowup(text)}
			onInterrupt={() => void session.interruptTurn()}
		/>
	</main>
</div>

{#if session.otherApprovalCount > 0}
	<p class="approval-queue-note" role="status">
		{session.otherApprovalCount} approval{session.otherApprovalCount === 1 ? "" : "s"} waiting
		on another thread. Switch threads to review.
	</p>
{/if}

<ApprovalDialog
	request={session.currentApproval}
	changes={session.approvalChanges}
	onDecision={(request: ApprovalRequest, decision) => session.respondApproval(request, decision)}
/>

<style>
	.shell {
		display: grid;
		grid-template-columns: minmax(16rem, 19rem) minmax(0, 1fr);
		height: 100dvh;
		min-height: 0;
	}

	.workspace {
		display: flex;
		min-width: 0;
		min-height: 0;
		flex-direction: column;
		outline: none;
	}

	.workspace-heading {
		display: flex;
		align-items: end;
		justify-content: space-between;
		flex-wrap: wrap;
		gap: var(--space-3);
		padding: var(--space-4) var(--space-6);
	}

	.workspace-heading p {
		margin: 0 0 var(--space-1);
		color: var(--dim);
		font-size: var(--size-caption);
	}

	.workspace-heading h2 {
		max-width: 48rem;
		margin: 0;
		overflow-wrap: break-word;
		font-size: var(--size-heading);
		font-weight: 650;
		line-height: 1.25;
		text-wrap: balance;
	}

	.error-banner {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
		margin: 0 var(--space-5) var(--space-3);
		border-inline-start: 2px solid var(--color-danger);
		background: var(--color-danger-surface);
		padding: var(--space-3);
	}

	.error-banner p {
		margin: 0;
		font-size: var(--text-body-small);
	}

	.error-banner button {
		flex: none;
		min-height: 32px;
		padding: var(--space-1) var(--space-3);
		font-size: var(--text-caption);
	}

	.approval-queue-note {
		margin: 0 var(--space-5) var(--space-3);
		border-inline-start: 2px solid var(--color-accent);
		background: var(--color-bg-inset);
		padding: var(--space-3);
		font-size: var(--text-body-small);
	}

	:global(.transcript) {
		flex: 1;
	}

	@media (max-width: 52rem) {
		.shell {
			grid-template-columns: minmax(0, 1fr);
			grid-template-rows: auto auto;
			height: auto;
			min-height: 100dvh;
		}

		:global(.sidebar) {
			max-height: 18rem;
		}
	}

	@media (max-width: 40rem) {
		.workspace-heading {
			padding: var(--space-3) var(--space-4);
		}

		.error-banner {
			align-items: start;
			margin-inline: var(--space-4);
		}
	}
</style>
