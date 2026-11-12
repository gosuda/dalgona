<script lang="ts">
import ApprovalDialog from "#lib/components/ApprovalDialog.svelte";
import Composer from "#lib/components/Composer.svelte";
import NoticeStack from "#lib/components/NoticeStack.svelte";
import StateTag from "#lib/components/StateTag.svelte";
import StatusBand from "#lib/components/StatusBand.svelte";
import ThreadList from "#lib/components/ThreadList.svelte";
import Transcript from "#lib/components/Transcript.svelte";
import { type StateKey, stateKey, stateWords } from "#lib/labels.ts";
import type { Session } from "#lib/session.svelte.ts";
import type { ApprovalRequest, TurnStatus } from "#lib/types.ts";

interface Props {
	session: Session;
}

let { session }: Props = $props();
const waitingThreadIds = $derived(
	new Set(session.approvals.map((request) => request.threadId)),
);
const activeThread = $derived(
	session.threads.find((thread) => thread.id === session.activeThreadId),
);
const openThreadFlags = $derived(activeThread?.activeFlags ?? []);
const folder = $derived(
	(activeThread?.cwd ?? "").split(/[\\/]/).findLast((part) => part !== "") ??
		"",
);
const usage = $derived(
	session.activeThreadId === null
		? null
		: (session.usage[session.activeThreadId] ?? null),
);

const turnStates: Record<TurnStatus, StateKey> = {
	inProgress: "inProgress",
	completed: "completed",
	interrupted: "interrupted",
	failed: "failed",
};
const headerState: StateKey = $derived(
	session.connection === "disconnected"
		? "disconnected"
		: session.transcript.turnStatus === null
			? "idle"
			: session.transcript.turnStatus === "inProgress"
				? (stateKey("active", openThreadFlags) ?? "inProgress")
				: turnStates[session.transcript.turnStatus],
);
const titleText = $derived(
	`dalgona · ${session.activeTitle} · ${stateWords[headerState]}`,
);
</script>

<svelte:head>
	<title>{titleText}</title>
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
		<header class="thread-head">
			<div class="thread-title">
				<h2>{session.activeTitle}</h2>
				<StateTag
					state={headerState}
					startedAtMs={session.transcript.turnStartedAtMs}
					retryRemainingMs={session.retryRemainingMs}
					tick={session.transcriptRevision}
					point={session.running && session.currentApproval === null}
				/>
			</div>
			<StatusBand
				status={session.status}
				models={session.models}
				selectedModel={session.selectedModel}
				pickerDisabled={session.connection !== "connected"}
				usage={usage}
				statusHide={session.statusHide}
				folder={folder}
				queued={session.queuedFollowups.length}
				onModel={(model) => void session.changeModel(model)}
			/>
		</header>

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
			verbosity={session.verbosity}
			readOutput={(itemId, offset) => session.readItemOutput(itemId, offset)}
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

	.thread-head {
		display: flex;
		align-items: flex-start;
		flex-wrap: wrap;
		gap: var(--space-3) var(--space-6);
		border-block-end: 1px solid var(--line);
		background: var(--surface);
		padding: var(--space-4) var(--space-6);
	}

	.thread-title {
		display: flex;
		flex: 1 1 16rem;
		min-width: 0;
		flex-direction: column;
		gap: var(--space-2);
	}

	.thread-title h2 {
		max-width: 48rem;
		margin: 0;
		overflow-wrap: anywhere;
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
		.thread-head {
			padding: var(--space-3) var(--space-4);
		}
	}
</style>
