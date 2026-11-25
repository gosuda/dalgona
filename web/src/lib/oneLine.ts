import type { Attachment } from "svelte/attachments";
import { isLineBreak } from "#lib/text/validate.ts";

export function hasLineBreak(text: string): boolean {
	for (const char of text) {
		if (isLineBreak(char.codePointAt(0) ?? 0)) return true;
	}
	return false;
}

export const oneLineRefusal = "This field takes one line. Nothing was pasted.";

export function oneLine(
	onRefuse: (message: string) => void,
): Attachment<HTMLInputElement> {
	return (node) => {
		const refuse = (event: Event, text: string | undefined): void => {
			if (text === undefined || !hasLineBreak(text)) return;
			event.preventDefault();
			onRefuse(oneLineRefusal);
		};
		const paste = (event: ClipboardEvent): void => {
			refuse(event, event.clipboardData?.getData("text/plain"));
		};
		const drop = (event: DragEvent): void => {
			refuse(event, event.dataTransfer?.getData("text/plain"));
		};
		node.addEventListener("paste", paste);
		node.addEventListener("drop", drop);
		return () => {
			node.removeEventListener("paste", paste);
			node.removeEventListener("drop", drop);
		};
	};
}
