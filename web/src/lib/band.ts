import type { TokenUsage } from "./types";

export const folderGlyph = "\u{F0256}";
export const queueGlyph = "\u{F051F}";

const effortGlyphs: readonly string[] = [
	"\u{F0873}",
	"\u{F0875}",
	"\u{F029A}",
	"\u{F0874}",
];

export const approvalGlyphs: Readonly<Partial<Record<string, string>>> = {
	ask: "\u{F0E47}",
	auto: "\u{F167A}",
	yolo: "\u{F0238}",
};

export function contextFraction(usage: TokenUsage): number | null {
	const window = usage.contextWindow;
	if (window === null || window <= 0) {
		return null;
	}
	const last = usage.last;
	const used =
		last.inputTokens +
		last.cachedInputTokens +
		last.cacheWriteInputTokens +
		last.outputTokens;
	return Math.min(1, Math.max(0, used / window));
}

export function moonGlyph(fraction: number): string {
	return String.fromCodePoint(0xe38d + Math.round(fraction * 14));
}

export function effortGlyph(rank: number, levels: number): string {
	return effortGlyphs[Math.round((rank * 3) / (levels - 1))] ?? "";
}
