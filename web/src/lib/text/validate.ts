import { paletteBits, scalarPalette, scalarRuns } from "./tables";

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

const whiteSpaceFlag = 1 << 7;
const ignorableFlag = 1 << 8;

function propertyOf(cp: number): number {
	if (cp < 0 || cp > 0x10ffff) return 0;
	const target = (cp + 1) << paletteBits;
	let lo = 0;
	let hi = scalarRuns.length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if ((scalarRuns[mid] ?? 0) < target) lo = mid + 1;
		else hi = mid;
	}
	if (lo === 0) return 0;
	const palette = (scalarRuns[lo - 1] ?? 0) & ((1 << paletteBits) - 1);
	return (scalarPalette[palette] ?? [0, 0, 0])[0] ?? 0;
}

export function isWhiteSpace(cp: number): boolean {
	return (propertyOf(cp) & whiteSpaceFlag) !== 0;
}

export function isDefaultIgnorable(cp: number): boolean {
	return (propertyOf(cp) & ignorableFlag) !== 0;
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
