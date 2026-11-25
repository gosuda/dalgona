import { paletteBits, scalarPalette, scalarRuns } from "./tables";

export interface DisplaySegment {
	readonly text: string;
	readonly escape: string | null;
}

export type DisplayReveal = "controls" | "invisible" | "whitespace";

const bidiControls: ReadonlySet<number> = new Set([
	0x061c, 0x200e, 0x200f, 0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066,
	0x2067, 0x2068, 0x2069,
]);

const ESC = 0x1b;
const BEL = 0x07;
const BACKSLASH = 0x5c;
const SEMICOLON = 0x3b;
const CAN = 0x18;
const SUB = 0x1a;

const WHITE_SPACE = 1 << 7;
const DEFAULT_IGNORABLE = 1 << 8;

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

function scalarAt(text: string, index: number): number {
	return text.codePointAt(index) ?? 0;
}

function scalarWidth(scalar: number): number {
	return scalar > 0xffff ? 2 : 1;
}

function within(unit: number, low: number, high: number): boolean {
	return unit >= low && unit <= high;
}

function isWhiteSpaceScalar(scalar: number): boolean {
	return (propertyOf(scalar) & WHITE_SPACE) !== 0;
}

function isInvisibleScalar(scalar: number): boolean {
	if (scalar <= 0x1f || within(scalar, 0x7f, 0x9f)) return true;
	return (propertyOf(scalar) & (WHITE_SPACE | DEFAULT_IGNORABLE)) !== 0;
}

function needsEscape(scalar: number, reveal: DisplayReveal): boolean {
	if (scalar === 0x09 || scalar === 0x0a) return false;
	if (scalar <= 0x1f || within(scalar, 0x7f, 0x9f)) return true;
	if (bidiControls.has(scalar) || within(scalar, 0xd800, 0xdfff)) return true;
	if (reveal === "controls") return false;
	if ((propertyOf(scalar) & DEFAULT_IGNORABLE) !== 0) return true;
	return reveal === "whitespace" && isWhiteSpaceScalar(scalar);
}

function codePointLabel(scalar: number): string {
	return `U+${scalar.toString(16).toUpperCase().padStart(4, "0")}`;
}

export function displaySegments(
	text: string,
	reveal: DisplayReveal,
): readonly DisplaySegment[] {
	const segments: DisplaySegment[] = [];
	let plainStart = 0;
	for (let index = 0; index < text.length; ) {
		const scalar = scalarAt(text, index);
		const end = index + scalarWidth(scalar);
		if (needsEscape(scalar, reveal)) {
			if (index > plainStart) {
				segments.push({ text: text.slice(plainStart, index), escape: null });
			}
			segments.push({
				text: text.slice(index, end),
				escape: codePointLabel(scalar),
			});
			plainStart = end;
		}
		index = end;
	}
	if (plainStart < text.length) {
		segments.push({ text: text.slice(plainStart), escape: null });
	}
	return segments;
}

export function invisibleKey(text: string): string {
	return displaySegments(text, "invisible")
		.filter((segment) => segment.escape === null)
		.map((segment) => segment.text)
		.join("");
}

export function collapsedDisplayKey(text: string): string {
	return text.replace(/[ \t\n\f\r]+/g, " ").replace(/^ | $/g, "");
}

function invisibleEnd(text: string, index: number): number | null {
	const scalar = scalarAt(text, index);
	return isInvisibleScalar(scalar) ? index + scalarWidth(scalar) : null;
}

function escapeEnd(text: string, index: number): number | null {
	if (text.charCodeAt(index) !== ESC) return null;
	const introducer = text.charCodeAt(index + 1);
	const body = index + 2;
	switch (introducer) {
		case 0x5b:
			return headerEnd(text, body);
		case 0x50:
			return dcsEnd(text, body);
		case 0x5d:
			return oscEnd(text, body);
		case 0x58:
		case 0x5e:
		case 0x5f:
			return payloadEnd(text, body, false);
		default:
			return shortEscapeEnd(text, index + 1);
	}
}

function skip(text: string, index: number, low: number, high: number): number {
	let next = index;
	while (next < text.length && within(text.charCodeAt(next), low, high)) {
		next += 1;
	}
	return next;
}

function headerEnd(text: string, index: number): number | null {
	const final = skip(text, skip(text, index, 0x30, 0x3f), 0x20, 0x2f);
	return within(text.charCodeAt(final), 0x40, 0x7e) ? final + 1 : null;
}

function shortEscapeEnd(text: string, index: number): number | null {
	const final = skip(text, index, 0x20, 0x2f);
	return within(text.charCodeAt(final), 0x30, 0x7e) ? final + 1 : null;
}

function dcsEnd(text: string, index: number): number | null {
	const payload = headerEnd(text, index);
	return payload === null ? null : payloadEnd(text, payload, false);
}

function oscEnd(text: string, index: number): number | null {
	const selectorEnd = skip(text, index, 0x30, 0x39);
	if (selectorEnd === index) return null;
	const unit = text.charCodeAt(selectorEnd);
	if (unit === SEMICOLON) return payloadEnd(text, selectorEnd + 1, true);
	return terminatorEnd(text, selectorEnd, true);
}

function terminatorEnd(
	text: string,
	index: number,
	bellEnds: boolean,
): number | null {
	const unit = text.charCodeAt(index);
	if (bellEnds && unit === BEL) return index + 1;
	if (unit === ESC && text.charCodeAt(index + 1) === BACKSLASH) {
		return index + 2;
	}
	return null;
}

function payloadEnd(
	text: string,
	index: number,
	bellEnds: boolean,
): number | null {
	for (let next = index; next < text.length; next += 1) {
		const end = terminatorEnd(text, next, bellEnds);
		if (end !== null) return end;
		const unit = text.charCodeAt(next);
		if (unit === ESC || unit === CAN || unit === SUB) return null;
	}
	return null;
}

export function isVisuallyBlank(text: string): boolean {
	if (!text.isWellFormed()) return false;
	let index = 0;
	while (index < text.length) {
		const next = escapeEnd(text, index) ?? invisibleEnd(text, index);
		if (next === null) return false;
		index = next;
	}
	return true;
}
