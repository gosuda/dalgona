import type { Attachment } from "svelte/attachments";

let lastFocused: HTMLElement | null = null;

export function messageOrTranscript(): HTMLElement | undefined {
	const prompt = document.getElementById("prompt");
	if (prompt !== null && !prompt.matches(":disabled")) return prompt;
	return document.getElementById("transcript") ?? undefined;
}

export function handoffFocus(
	fallback: () => HTMLElement | undefined,
	options?: FocusOptions,
): Attachment<HTMLElement> {
	return (node) => {
		const track = (): void => {
			lastFocused = node;
		};
		node.addEventListener("focusin", track);
		return () => {
			node.removeEventListener("focusin", track);
			if (lastFocused !== node) return;
			queueMicrotask(() => {
				const active = document.activeElement;
				if (active === null || active === document.body) {
					fallback()?.focus(options);
				}
			});
		};
	};
}
