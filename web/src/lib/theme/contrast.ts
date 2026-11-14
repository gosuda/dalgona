import type { Hex } from "./palette";

function channel(value: number): number {
	const scaled = value / 255;
	return scaled <= 0.03928 ? scaled / 12.92 : ((scaled + 0.055) / 1.055) ** 2.4;
}

function relativeLuminance(hex: Hex): number {
	const red = channel(Number.parseInt(hex.slice(1, 3), 16));
	const green = channel(Number.parseInt(hex.slice(3, 5), 16));
	const blue = channel(Number.parseInt(hex.slice(5, 7), 16));
	return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

export function contrastRatio(a: Hex, b: Hex): number {
	const first = relativeLuminance(a);
	const second = relativeLuminance(b);
	const lighter = Math.max(first, second);
	const darker = Math.min(first, second);
	return (lighter + 0.05) / (darker + 0.05);
}
