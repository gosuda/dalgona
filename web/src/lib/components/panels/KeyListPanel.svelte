<script lang="ts">
import Panel from "#lib/components/panels/Panel.svelte";

interface Props {
	readonly open: boolean;
	readonly onClose: () => void;
}

interface KeyRow {
	readonly keys: string;
	readonly action: string;
}

interface KeyGroup {
	readonly title: string;
	readonly rows: readonly KeyRow[];
}

let { open, onClose }: Props = $props();

const groups: readonly KeyGroup[] = [
	{
		title: "Composer",
		rows: [
			{ keys: "Enter", action: "Send, or steer while a turn runs" },
			{ keys: "Shift+Enter", action: "New line" },
			{ keys: "Alt+Enter", action: "Queue a follow-up while a turn runs" },
			{ keys: "ArrowUp, ArrowDown", action: "Recall sent messages" },
			{ keys: "Escape twice", action: "Interrupt a running turn" },
			{ keys: "/", action: "Open the command list" },
			{ keys: "//", action: "Send a message that begins with /" },
		],
	},
	{
		title: "Commands and panels",
		rows: [
			{ keys: "ArrowUp, ArrowDown", action: "Move in the command list" },
			{ keys: "Enter, Tab", action: "Open the highlighted command" },
			{ keys: "Escape", action: "Close the command list or a panel" },
			{ keys: "?", action: "Open this key list" },
		],
	},
	{
		title: "Transcript",
		rows: [{ keys: "Ctrl+O", action: "Open the full text of the latest row" }],
	},
	{
		title: "Requests",
		rows: [
			{ keys: "y", action: "Accept" },
			{ keys: "a", action: "Accept for session" },
			{ keys: "n", action: "Decline" },
			{ keys: "Escape", action: "Cancel and interrupt, or dismiss a question" },
		],
	},
];
</script>

<Panel title="Keys" {open} {onClose}>
	{#snippet children()}
		{#each groups as group (group.title)}
			<section class="group">
				<h3>{group.title}</h3>
				<dl>
					{#each group.rows as row (row.keys)}
						<div class="row">
							<dt><kbd>{row.keys}</kbd></dt>
							<dd>{row.action}</dd>
						</div>
					{/each}
				</dl>
			</section>
		{/each}
	{/snippet}
</Panel>

<style>
	.group {
		margin-block-end: var(--space-4);
	}

	h3 {
		margin: 0 0 var(--space-2);
		color: var(--dim);
		font-size: var(--size-caption);
		font-weight: var(--weight-title);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	dl {
		margin: 0;
	}

	.row {
		display: grid;
		grid-template-columns: minmax(10rem, 14rem) minmax(0, 1fr);
		gap: var(--space-3);
		padding-block: var(--space-2);
		border-block-end: var(--border-width) solid var(--line);
	}

	dt {
		font-family: var(--face-mono);
	}

	dd {
		margin: 0;
		overflow-wrap: anywhere;
	}
</style>
