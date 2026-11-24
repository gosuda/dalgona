import { whiteSpaceRanges } from "./tables";

export type TextReason =
	| "invalid_encoding"
	| "text_too_large"
	| "line_break_not_allowed"
	| "disallowed_control"
	| "invalid_offset";

export const answerScalars = 2000;
export const searchTermScalars = 3;

export interface TextField {
	readonly singleLine?: boolean;
	readonly maxScalars?: number;
}

export function checkText(
	text: string,
	field: TextField = {},
): TextReason | undefined {
	let scalars = 0;
	for (let i = 0; i < text.length; i++) {
		const unit = text.charCodeAt(i);
		let cp = unit;
		if (unit >= 0xd800 && unit <= 0xdbff) {
			const low = text.charCodeAt(i + 1);
			if (!(low >= 0xdc00 && low <= 0xdfff)) return "invalid_encoding";
			cp = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
			i++;
		} else if (unit >= 0xdc00 && unit <= 0xdfff) {
			return "invalid_encoding";
		}
		scalars++;
		if (field.maxScalars !== undefined && scalars > field.maxScalars)
			return "text_too_large";
		if (cp === 0) return "disallowed_control";
		if (field.singleLine && isLineBreak(cp)) return "line_break_not_allowed";
	}
	return undefined;
}

export function scalarCount(text: string): number {
	let n = 0;
	for (const _ of text) n++;
	return n;
}

export function isLineBreak(cp: number): boolean {
	return (
		cp === 0x0a ||
		cp === 0x0d ||
		cp === 0x0b ||
		cp === 0x0c ||
		cp === 0x85 ||
		cp === 0x2028 ||
		cp === 0x2029
	);
}

export function newlinesToLF(text: string): string {
	return text.includes("\r") ? text.replace(/\r\n?/g, "\n") : text;
}

export function isWhiteSpace(cp: number): boolean {
	for (let i = 0; i < whiteSpaceRanges.length; i += 2) {
		if (cp < (whiteSpaceRanges[i] ?? 0)) return false;
		if (cp <= (whiteSpaceRanges[i + 1] ?? 0)) return true;
	}
	return false;
}

export function isBlank(text: string): boolean {
	for (const ch of text) {
		if (!isWhiteSpace(ch.codePointAt(0) as number)) return false;
	}
	return true;
}

export function searchEligible(text: string): boolean {
	let terms = 0;
	let current = 0;
	for (const ch of text) {
		if (isWhiteSpace(ch.codePointAt(0) as number)) {
			if (current > 0 && current < searchTermScalars) return false;
			if (current > 0) terms++;
			current = 0;
		} else {
			current++;
		}
	}
	if (current > 0 && current < searchTermScalars) return false;
	return terms + (current > 0 ? 1 : 0) > 0;
}
