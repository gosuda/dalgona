<script lang="ts">
import {
	type PanelKind,
	type SlashCommand,
	slashCommands,
} from "#lib/commands.ts";
import CompletionList from "#lib/components/CompletionList.svelte";

interface Props {
	readonly id: string;
	readonly field: HTMLTextAreaElement | undefined;
	readonly query: string;
	active: number;
	readonly onOpen: (kind: PanelKind) => void;
	readonly onClose: () => void;
}

let {
	id,
	field,
	query,
	active = $bindable(0),
	onOpen,
	onClose,
}: Props = $props();

const matches = $derived(
	slashCommands.filter((command) =>
		command.name.startsWith(query.toLowerCase()),
	),
);
const highlighted = $derived(
	matches.length === 0 ? 0 : Math.min(active, matches.length - 1),
);
const items = $derived(
	matches.map((command) => ({
		id: command.name,
		primary: `/${command.name}`,
		secondary: command.summary,
	})),
);

$effect(() => {
	void query;
	active = 0;
});

function move(step: number): void {
	const count = matches.length;
	if (count === 0) return;
	active = (highlighted + step + count) % count;
}

function onKey(event: KeyboardEvent): void {
	if (event.isComposing) return;
	if (event.key === "Escape") {
		event.preventDefault();
		onClose();
		return;
	}
	if (event.key === "ArrowDown" || event.key === "ArrowUp") {
		event.preventDefault();
		move(event.key === "ArrowDown" ? 1 : -1);
		return;
	}
	const opens =
		(event.key === "Enter" || event.key === "Tab") &&
		!event.shiftKey &&
		!event.altKey;
	const command: SlashCommand | undefined = matches[highlighted];
	if (!opens || command === undefined) return;
	event.preventDefault();
	onOpen(command.panel);
}

$effect(() => {
	const node = field;
	if (node === undefined) return;
	node.addEventListener("keydown", onKey);
	return () => node.removeEventListener("keydown", onKey);
});
</script>

<CompletionList {id} label="Commands" {items} active={highlighted} />
{#if matches.length === 0}
	<p class="no-match">
		No command named {query}. Start with // to send a message that begins with /.
	</p>
{/if}

<style>
	.no-match {
		margin: var(--space-2) 0 0;
		color: var(--dim);
		font-size: var(--size-caption);
	}
</style>
