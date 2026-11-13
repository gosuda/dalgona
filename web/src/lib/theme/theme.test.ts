import { describe, expect, test } from "vitest";
import { parseThemeSetting, resolveTheme, tokenCss } from "./theme";

describe("theme resolution", () => {
	test("auto follows the system setting", () => {
		expect(resolveTheme("auto", true)).toBe("night");
		expect(resolveTheme("auto", false)).toBe("day");
	});

	test("an explicit theme ignores the system setting", () => {
		expect(resolveTheme("night", false)).toBe("night");
		expect(resolveTheme("day", true)).toBe("day");
	});

	test("auto-soft follows the system setting within the soft pair", () => {
		expect(resolveTheme("auto-soft", true)).toBe("soft-night");
		expect(resolveTheme("auto-soft", false)).toBe("soft-day");
	});

	test("unknown values fall back to auto", () => {
		expect(parseThemeSetting("sepia")).toBe("auto");
		expect(parseThemeSetting(undefined)).toBe("auto");
		expect(parseThemeSetting("soft-night")).toBe("soft-night");
	});

	test("token css carries every concrete theme and the hue rules", () => {
		const css = tokenCss();
		expect(css).toContain(':root[data-theme="soft-night"]');
		expect(css).toContain('[data-hue="3"]');
		expect(css).toContain("(prefers-color-scheme: dark)");
		expect(css).toContain("--face-text:");
	});
});
