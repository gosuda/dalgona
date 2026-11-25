import { isBlank, isLineBreak, newlinesToLF, searchEligible } from "./validate";

export type TextFault =
	| "invalid_encoding"
	| "disallowed_control"
	| "line_break_not_allowed";

export const faultNotices: Readonly<Record<TextFault, string>> = {
	invalid_encoding:
		"This text has an incomplete character. Retype the last character.",
	disallowed_control: "This text has a NUL character. Remove it to continue.",
	line_break_not_allowed:
		"This field takes one line. Paste or drop text without line breaks.",
};

export function multilineText(text: string): string {
	return newlinesToLF(text);
}

export function textFault(text: string): TextFault | null {
	if (!text.isWellFormed()) return "invalid_encoding";
	if (text.includes("\0")) return "disallowed_control";
	return null;
}

export function singleLineFault(text: string): TextFault | null {
	const fault = textFault(text);
	if (fault !== null) return fault;
	for (const char of text) {
		if (isLineBreak(char.codePointAt(0) ?? 0)) return "line_break_not_allowed";
	}
	return null;
}

export function isWhitespaceOnly(text: string): boolean {
	return isBlank(text);
}

export function searchEligibleChecked(query: string): boolean {
	if (textFault(query) !== null) return false;
	return searchEligible(query);
}

export function refuseSingleLineInsertion(
	event: ClipboardEvent | DragEvent,
): TextFault | null {
	const data =
		"clipboardData" in event ? event.clipboardData : event.dataTransfer;
	const fault = singleLineFault(data?.getData("text/plain") ?? "");
	if (fault !== null) event.preventDefault();
	return fault;
}
