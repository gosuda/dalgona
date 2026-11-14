export type PanelKind =
	| "model"
	| "settings"
	| "login"
	| "resume"
	| "new"
	| "compact"
	| "fork"
	| "tree"
	| "rewind"
	| "advisor"
	| "review"
	| "keys";

export interface SlashCommand {
	readonly name: string;
	readonly summary: string;
	readonly panel: PanelKind;
}

export const slashCommands: readonly SlashCommand[] = [
	{ name: "model", summary: "Choose a model or preset", panel: "model" },
	{
		name: "settings",
		summary:
			"Change theme, verbosity, approval, status items, edit review, and notifications",
		panel: "settings",
	},
	{ name: "login", summary: "Log in to a provider", panel: "login" },
	{ name: "resume", summary: "Open an earlier thread", panel: "resume" },
	{ name: "new", summary: "Start a new thread", panel: "new" },
	{ name: "compact", summary: "Compact this thread", panel: "compact" },
	{ name: "fork", summary: "Fork this thread at a turn", panel: "fork" },
	{
		name: "tree",
		summary: "Move to another branch of this thread",
		panel: "tree",
	},
	{ name: "revert", summary: "Rewind to before a turn", panel: "rewind" },
	{ name: "advisor", summary: "Show or change the advisor", panel: "advisor" },
	{ name: "review", summary: "Review changes", panel: "review" },
	{ name: "help", summary: "Show keys", panel: "keys" },
];

export const panelsRefusedWhileRunning: Partial<Record<PanelKind, true>> = {
	new: true,
	compact: true,
	fork: true,
	tree: true,
	rewind: true,
};

type ComposerInput =
	| { readonly kind: "message"; readonly text: string }
	| { readonly kind: "command"; readonly query: string }
	| { readonly kind: "shell"; readonly command: string };

export function parseComposerText(text: string): ComposerInput {
	if (text.startsWith("!")) {
		return { kind: "shell", command: text.slice(1).trim() };
	}
	if (text.startsWith("//")) {
		return { kind: "message", text: text.slice(1) };
	}
	if (!text.startsWith("/")) {
		return { kind: "message", text };
	}
	const word = text.split(/\s/, 1)[0] ?? "";
	if (word.indexOf("/", 1) !== -1) {
		return { kind: "message", text };
	}
	return { kind: "command", query: text.slice(1) };
}
