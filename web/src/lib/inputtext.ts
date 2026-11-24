import { isImeKey } from "./ime";
import { propertyRuns } from "./unicode-properties";

export type TextFault =
	| "invalid_encoding"
	| "disallowed_control"
	| "line_break_not_allowed";

export interface TextRange {
	readonly start: number;
	readonly end: number;
}

export interface DisplaySegment {
	readonly text: string;
	readonly escape: string | null;
}

export type DisplayReveal = "controls" | "invisible" | "whitespace";

type TextField = HTMLInputElement | HTMLTextAreaElement;

const bidiControls: ReadonlySet<number> = new Set([
	0x061c, 0x200e, 0x200f, 0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066,
	0x2067, 0x2068, 0x2069,
]);

export const faultNotices: Readonly<Record<TextFault, string>> = {
	invalid_encoding:
		"This text has an incomplete character. Retype the last character.",
	disallowed_control: "This text has a NUL character. Remove it to continue.",
	line_break_not_allowed:
		"This field takes one line. Paste or drop text without line breaks.",
};

const ESC = 0x1b;
const BEL = 0x07;
const BACKSLASH = 0x5c;
const SEMICOLON = 0x3b;
const CAN = 0x18;
const SUB = 0x1a;

const CLASS_MASK = 0x0f;
const CR = 1;
const LF = 2;
const CONTROL = 3;
const EXTEND = 4;
const ZWJ = 5;
const REGIONAL_INDICATOR = 6;
const PREPEND = 7;
const SPACING_MARK = 8;
const HANGUL_L = 9;
const HANGUL_V = 10;
const HANGUL_T = 11;
const HANGUL_LV = 12;
const HANGUL_LVT = 13;
const EXTENDED_PICTOGRAPHIC = 1 << 4;
const INCB_MASK = 3 << 5;
const INCB_LINKER = 1 << 5;
const INCB_CONSONANT = 2 << 5;
const INCB_EXTEND = 3 << 5;
const WHITE_SPACE = 1 << 7;
const DEFAULT_IGNORABLE = 1 << 8;

const EMOJI_NONE = 0;
const EMOJI_PICTOGRAPHIC = 1;
const EMOJI_JOINED = 2;

const hangulFollowers: readonly number[] = [
	0,
	0,
	0,
	0,
	0,
	0,
	0,
	0,
	0,
	(1 << HANGUL_L) | (1 << HANGUL_V) | (1 << HANGUL_LV) | (1 << HANGUL_LVT),
	(1 << HANGUL_V) | (1 << HANGUL_T),
	1 << HANGUL_T,
	(1 << HANGUL_V) | (1 << HANGUL_T),
	1 << HANGUL_T,
];

interface Cluster {
	lastClass: number;
	conjunctLinked: boolean;
	emoji: number;
	regionalIndicators: number;
}

interface PropertyTable {
	readonly starts: Uint32Array;
	readonly values: Uint16Array;
}

const lineBreak = /[\n\v\f\r\u0085\u2028\u2029]/u;
const properties = decodeRuns(propertyRuns);

function decodeRuns(runs: string): PropertyTable {
	const entries = runs.split(",");
	const starts = new Uint32Array(entries.length);
	const values = new Uint16Array(entries.length);
	let start = 0;
	entries.forEach((entry, index) => {
		const [delta = "", value = ""] = entry.split(".");
		start += Number.parseInt(delta, 36);
		starts[index] = start;
		values[index] = Number.parseInt(value, 36);
	});
	return { starts, values };
}

function propertyOf(scalar: number): number {
	let low = 0;
	let high = properties.starts.length - 1;
	while (low < high) {
		const middle = (low + high + 1) >>> 1;
		if ((properties.starts[middle] ?? 0) <= scalar) low = middle;
		else high = middle - 1;
	}
	return properties.values[low] ?? 0;
}

function scalarAt(text: string, index: number): number {
	return text.codePointAt(index) ?? 0;
}

function scalarWidth(scalar: number): number {
	return scalar > 0xffff ? 2 : 1;
}

function scalarStart(text: string, index: number): number {
	const unit = text.charCodeAt(index);
	const previous = text.charCodeAt(index - 1);
	const splitsPair =
		within(unit, 0xdc00, 0xdfff) && within(previous, 0xd800, 0xdbff);
	return splitsPair ? index - 1 : index;
}

function isWhiteSpaceScalar(scalar: number): boolean {
	return (propertyOf(scalar) & WHITE_SPACE) !== 0;
}

function isInvisibleScalar(scalar: number): boolean {
	if (scalar <= 0x1f || within(scalar, 0x7f, 0x9f)) return true;
	return (propertyOf(scalar) & (WHITE_SPACE | DEFAULT_IGNORABLE)) !== 0;
}

export function multilineText(text: string): string {
	return text.replace(/\r\n?/g, "\n");
}

export function textFault(text: string): TextFault | null {
	if (!text.isWellFormed()) return "invalid_encoding";
	if (text.includes("\0")) return "disallowed_control";
	return null;
}

export function singleLineFault(text: string): TextFault | null {
	const fault = textFault(text);
	if (fault !== null) return fault;
	return lineBreak.test(text) ? "line_break_not_allowed" : null;
}

export function scalarCount(text: string): number {
	let count = 0;
	for (let index = 0; index < text.length; count += 1) {
		index += scalarWidth(scalarAt(text, index));
	}
	return count;
}

export function isWhitespaceOnly(text: string): boolean {
	for (let index = 0; index < text.length; ) {
		const scalar = scalarAt(text, index);
		if (!isWhiteSpaceScalar(scalar)) return false;
		index += scalarWidth(scalar);
	}
	return true;
}

function termLengths(text: string): readonly number[] {
	const lengths: number[] = [];
	let current = 0;
	for (let index = 0; index < text.length; ) {
		const scalar = scalarAt(text, index);
		index += scalarWidth(scalar);
		if (!isWhiteSpaceScalar(scalar)) {
			current += 1;
			continue;
		}
		if (current > 0) lengths.push(current);
		current = 0;
	}
	if (current > 0) lengths.push(current);
	return lengths;
}

export function searchEligible(query: string): boolean {
	if (textFault(query) !== null) return false;
	const lengths = termLengths(query);
	return lengths.length > 0 && lengths.every((length) => length >= 3);
}

function isControlClass(kind: number): boolean {
	return kind === CONTROL || kind === CR || kind === LF;
}

function joinsPair(previous: number, next: number): boolean {
	if (previous === CR) return next === LF;
	if (isControlClass(previous) || isControlClass(next)) return false;
	if (((hangulFollowers[previous] ?? 0) & (1 << next)) !== 0) return true;
	return (
		next === EXTEND ||
		next === ZWJ ||
		next === SPACING_MARK ||
		previous === PREPEND
	);
}

function startsWithoutContext(property: number): boolean {
	return (
		(property & EXTENDED_PICTOGRAPHIC) === 0 &&
		(property & INCB_MASK) !== INCB_CONSONANT &&
		(property & CLASS_MASK) !== REGIONAL_INDICATOR
	);
}

function emojiAfter(emoji: number, property: number): number {
	if ((property & EXTENDED_PICTOGRAPHIC) !== 0) return EMOJI_PICTOGRAPHIC;
	if (emoji !== EMOJI_PICTOGRAPHIC) return EMOJI_NONE;
	const kind = property & CLASS_MASK;
	if (kind === EXTEND) return EMOJI_PICTOGRAPHIC;
	return kind === ZWJ ? EMOJI_JOINED : EMOJI_NONE;
}

function extendCluster(cluster: Cluster, property: number): void {
	const conjunct = property & INCB_MASK;
	const kind = property & CLASS_MASK;
	cluster.conjunctLinked =
		conjunct === INCB_LINKER ||
		(cluster.conjunctLinked && conjunct === INCB_EXTEND);
	cluster.emoji = emojiAfter(cluster.emoji, property);
	cluster.regionalIndicators =
		kind === REGIONAL_INDICATOR ? cluster.regionalIndicators + 1 : 0;
	cluster.lastClass = kind;
}

function continuesCluster(cluster: Cluster, next: number): boolean {
	if (cluster.conjunctLinked && (next & INCB_MASK) === INCB_CONSONANT) {
		return true;
	}
	if (cluster.emoji === EMOJI_JOINED && (next & EXTENDED_PICTOGRAPHIC) !== 0) {
		return true;
	}
	return (
		(next & CLASS_MASK) === REGIONAL_INDICATOR &&
		cluster.regionalIndicators % 2 === 1
	);
}

function clusterEnd(text: string, start: number): number {
	const cluster: Cluster = {
		lastClass: 0,
		conjunctLinked: false,
		emoji: EMOJI_NONE,
		regionalIndicators: 0,
	};
	const first = scalarAt(text, start);
	extendCluster(cluster, propertyOf(first));
	let end = start + scalarWidth(first);
	while (end < text.length) {
		const scalar = scalarAt(text, end);
		const next = propertyOf(scalar);
		const joined =
			joinsPair(cluster.lastClass, next & CLASS_MASK) ||
			continuesCluster(cluster, next);
		if (!joined) break;
		extendCluster(cluster, next);
		end += scalarWidth(scalar);
	}
	return end;
}

function guaranteedBoundary(text: string, index: number): number {
	let position = scalarStart(text, index);
	while (position > 0) {
		const next = propertyOf(scalarAt(text, position));
		const before = scalarStart(text, position - 1);
		const previous = propertyOf(scalarAt(text, before)) & CLASS_MASK;
		if (startsWithoutContext(next) && !joinsPair(previous, next & CLASS_MASK)) {
			return position;
		}
		position = before;
	}
	return 0;
}

function clusterAt(text: string, index: number): TextRange {
	let start = guaranteedBoundary(text, index);
	let end = clusterEnd(text, start);
	while (end <= index) {
		start = end;
		end = clusterEnd(text, start);
	}
	return { start, end };
}

export function deletionRange(
	text: string,
	caret: number,
	direction: "backward" | "forward",
): TextRange | null {
	const index = direction === "backward" ? caret - 1 : caret;
	if (index < 0 || index >= text.length) return null;
	return clusterAt(text, index);
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

function deletionDirection(key: string): "backward" | "forward" | null {
	if (key === "Backspace") return "backward";
	return key === "Delete" ? "forward" : null;
}

export function selectDeletedCluster(
	event: KeyboardEvent,
	field: TextField,
): void {
	const direction = deletionDirection(event.key);
	if (direction === null || isImeKey(event)) return;
	if (event.altKey || event.ctrlKey || event.metaKey) return;
	if (event.key === "Delete" && event.shiftKey) return;
	const caret = field.selectionStart;
	if (caret === null || caret !== field.selectionEnd) return;
	const range = deletionRange(field.value, caret, direction);
	if (range !== null) field.setSelectionRange(range.start, range.end);
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

export function isBlank(text: string): boolean {
	if (!text.isWellFormed()) return false;
	let index = 0;
	while (index < text.length) {
		const next = escapeEnd(text, index) ?? invisibleEnd(text, index);
		if (next === null) return false;
		index = next;
	}
	return true;
}

function invisibleEnd(text: string, index: number): number | null {
	const scalar = scalarAt(text, index);
	return isInvisibleScalar(scalar) ? index + scalarWidth(scalar) : null;
}

function within(unit: number, low: number, high: number): boolean {
	return unit >= low && unit <= high;
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
