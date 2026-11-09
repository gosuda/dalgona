<script lang="ts">
import { advisorLabel } from "#lib/labels.ts";
import type {
	AdvisorSummary,
	ConnectionState,
	StatusInfo,
	TurnStatus,
} from "#lib/types.ts";

interface Props {
	status: StatusInfo;
	connection: ConnectionState;
	advisor: AdvisorSummary;
	turnStatus: TurnStatus | null;
	onReconnect: () => void;
}

let { status, connection, advisor, turnStatus, onReconnect }: Props = $props();
let advisorText = $derived(advisorLabel(advisor));

const connectionLabels: Record<ConnectionState, string> = {
	idle: "Not connected",
	connecting: "Connecting",
	connected: "Connected",
	disconnected: "Disconnected",
};

const turnLabels: Record<TurnStatus, string> = {
	inProgress: "Working",
	completed: "Turn completed",
	interrupted: "Turn interrupted",
	failed: "Turn failed",
};
</script>

<header class="status-bar">
	<div class="connection">
		<span class:online={connection === "connected"} class="connection-mark" aria-hidden="true"></span>
		<span role="status">{connectionLabels[connection]}</span>
		{#if turnStatus}
			<span aria-hidden="true">·</span>
			<span class:working={turnStatus === "inProgress"}>{turnLabels[turnStatus]}</span>
		{/if}
	</div>

	<dl>
		<div>
			<dt>Model</dt>
			<dd>{status.model}</dd>
		</div>
		<div>
			<dt>Approval</dt>
			<dd>{status.approval}</dd>
		</div>
		<div>
			<dt>Sandbox</dt>
			<dd>{status.sandbox}</dd>
		</div>
		<div>
			<dt>Runner</dt>
			<dd>{status.runner}</dd>
		</div>
		{#if advisorText}
			<div>
				<dt>Advisor</dt>
				<dd>{advisorText}</dd>
			</div>
		{/if}
	</dl>

	{#if connection === "disconnected"}
		<button class="quiet" onclick={onReconnect}>Reconnect</button>
	{/if}
</header>

<style>
	.status-bar {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--space-3) var(--space-5);
		border-block-end: 1px solid var(--color-border);
		background: var(--color-bg-surface);
		padding: var(--space-3) var(--space-5);
	}

	.connection {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--space-2);
		font-size: var(--text-body-small);
		font-weight: 600;
	}

	.connection-mark {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--color-danger);
	}

	.connection-mark.online {
		background: var(--color-success);
	}

	.working {
		color: var(--color-success);
	}

	dl {
		display: flex;
		flex: 1;
		flex-wrap: wrap;
		gap: var(--space-2) var(--space-5);
		margin: 0;
	}

	dl div {
		display: flex;
		align-items: baseline;
		gap: var(--space-1);
		min-width: 0;
	}

	dt {
		color: var(--color-text-secondary);
		font-size: var(--text-caption);
	}

	dd {
		margin: 0;
		overflow-wrap: break-word;
		font-size: var(--text-caption);
		font-weight: 600;
	}

	.status-bar button {
		min-height: 32px;
		padding: var(--space-1) var(--space-3);
		font-size: var(--text-caption);
	}
</style>
