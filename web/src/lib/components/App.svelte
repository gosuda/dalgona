<script lang="ts">
import ApprovalDialog from "#lib/components/ApprovalDialog.svelte";
import Composer from "#lib/components/Composer.svelte";
import ModelPicker from "#lib/components/ModelPicker.svelte";
import NoticeStack from "#lib/components/NoticeStack.svelte";
import StatusBar from "#lib/components/StatusBar.svelte";
import ThreadList from "#lib/components/ThreadList.svelte";
import Transcript from "#lib/components/Transcript.svelte";
import type { Session } from "#lib/session.svelte.ts";
import type { ApprovalRequest } from "#lib/types.ts";

interface Props {
	session: Session;
}

let { session }: Props = $props();
const waitingThreadIds = $derived(
	new Set(session.approvals.map((request) => request.threadId)),
);
const openThreadFlags = $derived(
	session.threads.find((thread) => thread.id === session.activeThreadId)
		?.activeFlags ?? [],
);
</script>

<svelte:head>
	<title>{session.activeTitle} · dalgona</title>
</svelte:head>

<a class="skip-link" href="#transcript">Skip to transcript</a>

<div class="shell">
	<ThreadList
		threads={session.threads}
		activeId={session.activeThreadId}
		loading={session.loadingThreads}
		connected={session.connection === "connected"}
		disabled={session.connection !== "connected"}
		starting={session.startingThread}
		waitingThreadIds={waitingThreadIds}
		completedThreadIds={session.completedThreadIds}
		onNew={() => void session.createThread()}
		onSelect={(threadId) => void session.selectThread(threadId)}
	/>

	<main id="main-content" class="workspace" tabindex="-1">
		<StatusBar
			status={session.status}
			connection={session.connection}
			advisor={session.advisor}
			turnStatus={session.transcript.turnStatus}
			turnStartedAtMs={session.transcript.turnStartedAtMs}
			retryRemainingMs={session.retryRemainingMs}
			point={session.running && session.currentApproval === null}
			activeFlags={openThreadFlags}
			revision={session.transcriptRevision}
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

		<NoticeStack
			notices={session.notices}
			waiting={session.waitingElsewhere}
			onDismiss={(id) => session.dismiss(id)}
			onOpenWaiting={(threadId) => void session.selectThread(threadId)}
			turnStatus={session.transcript.turnStatus}
			turnError={session.transcript.error}
			loadingTranscript={session.loadingTranscript}
			retryRemainingMs={session.retryRemainingMs}
			awaitingApproval={session.awaitingApproval}
		/>

		<Transcript
			threadId={session.activeThreadId}
			items={session.transcript.items}
			turnStatus={session.transcript.turnStatus}
			error={session.transcript.error}
			plan={session.transcript.plan}
			turnReview={session.transcript.turnReview}
			connected={session.connection === "connected"}
			childThreadIds={session.transcript.childThreadIds}
			threads={session.threads}
			userAgent={session.userAgent}
			starting={session.startingThread}
			onOpenThread={(threadId) => void session.selectThread(threadId)}
			onStart={() => void session.createThread()}
			loading={session.loadingTranscript}
			awaitingApproval={session.awaitingApproval}
		/>

		<Composer
			disabled={session.connection !== "connected" || session.activeThreadId === null || session.loadingTranscript}
			running={session.running}
			queuedCount={session.queuedFollowups.length}
			onMatch={(text) => session.matchKeywords(text)}
			onSend={(text) => void session.sendMessage(text)}
			onSteer={(text) => void session.steerMessage(text)}
			onFollowup={(text) => session.queueFollowup(text)}
			onInterrupt={() => void session.interruptTurn()}
		/>
	</main>
</div>

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
	}

	@media (max-width: 40rem) {
		.workspace-heading {
			padding: var(--space-3) var(--space-4);
		}
	}
</style>
