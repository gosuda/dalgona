import { describe, expect, test } from "vitest";
import { contrastRatio } from "./contrast";
import {
	concreteThemes,
	contrastRules,
	huePair,
	hues,
	palette,
} from "./palette";

describe("contrast", () => {
	test("matches WCAG reference ratios", () => {
		expect(contrastRatio("#000000", "#FFFFFF")).toBeCloseTo(21, 5);
		expect(contrastRatio("#FFFFFF", "#FFFFFF")).toBeCloseTo(1, 5);
		const grey = contrastRatio("#767676", "#FFFFFF");
		expect(grey).toBeGreaterThan(4.54);
		expect(grey).toBeLessThan(4.55);
	});

	test("every theme meets its contrast rules", () => {
		const failures: string[] = [];
		for (const theme of concreteThemes) {
			for (const rule of contrastRules) {
				const ratio = contrastRatio(
					palette[theme][rule.fg],
					palette[theme][rule.bg],
				);
				if (ratio < rule.min) {
					failures.push(
						`${theme} ${rule.fg} on ${rule.bg}: ${ratio.toFixed(2)} (want ${rule.min})`,
					);
				}
			}
		}
		expect(failures).toEqual([]);
	});

	test("every hue has legible ink on its point in both pairs", () => {
		const failures: string[] = [];
		for (const theme of concreteThemes) {
			const pair = huePair(theme);
			const ground = palette[theme].canvas;
			for (const hue of hues) {
				const colours = hue[pair];
				if (contrastRatio(colours.onPoint, colours.point) < 4.5) {
					failures.push(`${theme} ${hue.name} ink on point`);
				}
				if (contrastRatio(colours.point, ground) < 3) {
					failures.push(`${theme} ${hue.name} point on canvas`);
				}
			}
		}
		expect(failures).toEqual([]);
	});
});
