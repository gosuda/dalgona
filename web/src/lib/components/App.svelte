<script lang="ts">
import ApprovalCard from "#lib/components/ApprovalCard.svelte";
import Composer from "#lib/components/Composer.svelte";
import NoticeStack from "#lib/components/NoticeStack.svelte";
import KeyListPanel from "#lib/components/panels/KeyListPanel.svelte";
import LoginPanel from "#lib/components/panels/LoginPanel.svelte";
import ModelPanel from "#lib/components/panels/ModelPanel.svelte";
import SettingsPanel from "#lib/components/panels/SettingsPanel.svelte";
import ThreadPanels from "#lib/components/panels/ThreadPanels.svelte";
import QuestionCard from "#lib/components/QuestionCard.svelte";
import StateTag from "#lib/components/StateTag.svelte";
import StatusBand from "#lib/components/StatusBand.svelte";
import ThreadList from "#lib/components/ThreadList.svelte";
import Transcript from "#lib/components/Transcript.svelte";
import { threadHue } from "#lib/hue.ts";
import {
	type StateKey,
	stateKey,
	stateWords,
	threadTitle,
} from "#lib/labels.ts";
import { isQuestionRequest } from "#lib/question.ts";
import { requestsElsewhere } from "#lib/reducer.ts";
import type { Session } from "#lib/session.svelte.ts";
import type { TurnStatus } from "#lib/types.ts";

interface Props {
	session: Session;
}

let { session }: Props = $props();
let dockHeight = $state(0);
let announcement = $state("");
const waitingThreadIds = $derived(
	new Set(session.requests.map((request) => request.threadId)),
);
const activeThread = $derived(
	session.threads.find((thread) => thread.id === session.activeThreadId),
);
const openHue = $derived(
	session.activeThreadId === null
		? undefined
		: threadHue(session.activeThreadId),
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
const elsewhere = $derived(
	new Set(
		requestsElsewhere(
			session.requests,
			session.activeThreadId,
			session.transcript.childThreadIds,
		),
	),
);
const openRequests = $derived(
	session.requests.filter((request) => !elsewhere.has(request)),
);
const shownPosition = $derived(
	session.currentRequest === null
		? 0
		: openRequests.indexOf(session.currentRequest) + 1,
);
const subagentTitle = $derived.by(() => {
	const request = session.currentRequest;
	if (request === null || request.threadId === session.activeThreadId) {
		return null;
	}
	const thread = session.threads.find((entry) => entry.id === request.threadId);
	return thread === undefined ? request.threadId : threadTitle(thread);
});

function isTyping(target: EventTarget | null): boolean {
	return (
		target instanceof HTMLElement &&
		target.matches("input, textarea, select, [contenteditable]")
	);
}

function onWindowKeydown(event: KeyboardEvent): void {
	if (event.key !== "?" || event.defaultPrevented) return;
	if (event.ctrlKey || event.metaKey || event.altKey) return;
	if (session.panel !== null || isTyping(document.activeElement)) return;
	session.openPanel("keys");
}
</script>

<svelte:head>
	<title>{titleText}</title>
</svelte:head>
<svelte:window onkeydown={onWindowKeydown} />

<a class="skip-link" href="#transcript">Skip to transcript</a>

<div class="shell" style:--dock-height="{dockHeight}px">
	<ThreadList
		threads={session.threads}
		activeId={session.activeThreadId}
		loading={session.loadingThreads}
		hasMore={session.nextThreadCursor !== null}
		connected={session.connection === "connected"}
		disabled={session.connection !== "connected"}
		starting={session.startingThread}
		waitingThreadIds={waitingThreadIds}
		completedThreadIds={session.completedThreadIds}
		onNew={() => void session.createThread()}
		onSelect={(threadId) => void session.selectThread(threadId)}
		onLoadMore={() => void session.loadMoreThreads()}
	/>

	<main id="main-content" class="workspace" data-hue={openHue}>
		<header class="thread-head">
			<div class="thread-title">
				<h2 title={session.activeTitle}>{session.activeTitle}</h2>
				<StateTag
					state={headerState}
					startedAtMs={session.transcript.turnStartedAtMs}
					retryRemainingMs={session.retryRemainingMs}
					tick={session.transcriptRevision}
					point={session.running && session.currentRequest === null}
				/>
			</div>
			<StatusBand
				status={session.status}
				models={session.models}
				selectedModel={session.selectedModel}
				modelDisabled={session.connection !== "connected"}
				usage={usage}
				statusHide={session.statusHide}
				folder={folder}
				queued={session.queuedFollowups.length}
				onOpenModel={() => session.openPanel("model")}
			/>
		</header>

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
			timedOutQuestions={session.timedOutQuestions}
			verbosity={session.verbosity}
			readOutput={(itemId, offset) => session.readItemOutput(itemId, offset)}
			onRewind={(turnId) => session.openPanel("rewind", turnId)}
			retryRemainingMs={session.retryRemainingMs}
			reviewNote={session.reviewNote}
			onAnnounce={(text) => (announcement = text)}
		/>

		<div class="dock" class:deciding={session.currentRequest !== null} bind:clientHeight={dockHeight}>
			<NoticeStack
				notices={session.notices}
				waiting={session.waitingElsewhere}
				onDismiss={(id) => session.dismiss(id)}
				onOpenWaiting={(threadId) => void session.selectThread(threadId)}
				announcement={announcement}
			/>

			{#if session.currentRequest !== null && isQuestionRequest(session.currentRequest)}
				<QuestionCard
					request={session.currentRequest}
					position={shownPosition}
					total={openRequests.length}
					subagent={subagentTitle}
					onSubmit={(request, selections) => session.answerQuestion(request, selections)}
					onDismiss={(request) => session.cancelQuestion(request)}
				/>
			{:else if session.currentRequest !== null && !isQuestionRequest(session.currentRequest)}
				<ApprovalCard
					request={session.currentRequest}
					changes={session.approvalChanges}
					position={shownPosition}
					total={openRequests.length}
					subagent={subagentTitle}
					onDecision={(request, decision) => session.respondApproval(request, decision)}
				/>
			{/if}

			<Composer
				threadOpen={session.activeThreadId !== null}
				deciding={session.currentRequest !== null}
				disabled={session.connection !== "connected" || session.activeThreadId === null || session.loadingTranscript}
				running={session.running}
				followups={session.queuedFollowups}
				history={session.history}
				onEditFollowup={(index) => session.editFollowup(index)}
				onRemoveFollowup={(index) => session.removeFollowup(index)}
				onMatch={(text) => session.matchKeywords(text)}
				onFileSearch={(query) => session.searchFiles(query)}
				onSend={(text, attachments) => void session.sendMessage(text, attachments)}
				onSteer={(text) => void session.steerMessage(text)}
				onFollowup={(text) => session.queueFollowup(text)}
				onInterrupt={() => void session.interruptTurn()}
				retrying={session.retryRemainingMs !== null}
				acceptsImages={session.modelAcceptsImages}
				onOpenPanel={(kind) => session.openPanel(kind)}
				cwd={activeThread?.cwd ?? ""}
				onShell={(command) => session.runShell(command)}
			/>
		</div>
	</main>
</div>

<KeyListPanel open={session.panel === "keys"} onClose={() => session.closePanel()} />
<LoginPanel {session} open={session.panel === "login"} onClose={() => session.closePanel()} />
<ModelPanel {session} open={session.panel === "model"} onClose={() => session.closePanel()} />
<SettingsPanel
	{session}
	open={session.panel === "settings"}
	onClose={() => session.closePanel()}
/>
<ThreadPanels {session} />

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
		min-width: 0;
		max-width: 100%;
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
		display: -webkit-box;
		max-width: 48rem;
		margin: 0;
		overflow: hidden;
		overflow-wrap: anywhere;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		font-size: var(--size-heading);
		font-weight: 650;
		line-height: 1.25;
		text-wrap: balance;
	}

	.dock {
		display: flex;
		max-height: 100dvh;
		min-width: 0;
		flex-direction: column;
	}

	.dock.deciding {
		flex: 0 1 auto;
		overflow-y: auto;
		overscroll-behavior: contain;
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
			padding-block-end: var(--dock-height);
		}

		.dock {
			position: fixed;
			inset-inline: 0;
			inset-block-end: 0;
			max-height: calc(100dvh - 6rem);
			overflow-y: auto;
			background: var(--surface);
			border-block-start: 1px solid var(--line);
		}

		.dock.deciding {
			max-height: 100dvh;
		}
	}

	@media (max-width: 40rem) {
		.thread-head {
			padding: var(--space-3) var(--space-4);
		}
	}
</style>
