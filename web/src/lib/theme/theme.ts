import { type ConcreteTheme, huePair, hues, palette } from "./palette";
import { scale } from "./scale";

export const themeSettings = [
	"auto",
	"auto-soft",
	"night",
	"soft-night",
	"day",
	"soft-day",
] as const;

export type ThemeSetting = (typeof themeSettings)[number];

export const themeStorageKey = "dalgona.theme";

export function parseThemeSetting(value: unknown): ThemeSetting {
	return themeSettings.find((setting) => setting === value) ?? "auto";
}

export function resolveTheme(
	setting: ThemeSetting,
	prefersDark: boolean,
): ConcreteTheme {
	switch (setting) {
		case "auto":
			return prefersDark ? "night" : "day";
		case "auto-soft":
			return prefersDark ? "soft-night" : "soft-day";
		default:
			return setting;
	}
}

function kebab(name: string): string {
	return name.replace(/([a-z])([A-Z0-9])/g, "$1-$2").toLowerCase();
}

function declarations(values: Readonly<Record<string, string>>): string {
	return Object.entries(values)
		.map(([name, value]) => `--${kebab(name)}:${value};`)
		.join("");
}

interface ThemeBlock {
	readonly theme: ConcreteTheme;
	readonly selectors: readonly string[];
	readonly media?: string;
}

const themeBlocks: readonly ThemeBlock[] = [
	{
		theme: "day",
		selectors: [":root", ':root[data-theme="day"]', ':root[data-theme="auto"]'],
	},
	{
		theme: "soft-day",
		selectors: [
			':root[data-theme="soft-day"]',
			':root[data-theme="auto-soft"]',
		],
	},
	{ theme: "night", selectors: [':root[data-theme="night"]'] },
	{ theme: "soft-night", selectors: [':root[data-theme="soft-night"]'] },
	{
		theme: "night",
		media: "(prefers-color-scheme: dark)",
		selectors: [':root[data-theme="auto"]'],
	},
	{
		theme: "soft-night",
		media: "(prefers-color-scheme: dark)",
		selectors: [':root[data-theme="auto-soft"]'],
	},
];

const EMPTY_VIEW_HUE = 1;

function themeCss(block: ThemeBlock): string {
	const colours = palette[block.theme];
	const pair = huePair(block.theme);
	const scheme = block.theme.endsWith("night") ? "dark" : "light";
	const select = (suffix: string): string =>
		block.selectors.map((selector) => `${selector}${suffix}`).join(",");
	const hueRules = hues
		.map(
			(hue, index) =>
				`${select(` [data-hue="${index}"]`)}{--point:${hue[pair].point};--on-point:${hue[pair].onPoint}}`,
		)
		.join("");
	const emptyHue = hues[EMPTY_VIEW_HUE]?.[pair];
	const defaults = emptyHue
		? { point: emptyHue.point, onPoint: emptyHue.onPoint }
		: {};
	const body = `${select("")}{color-scheme:${scheme};${declarations({ ...colours, ...defaults })}}${hueRules}`;
	return block.media === undefined ? body : `@media ${block.media}{${body}}`;
}

export function tokenCss(): string {
	const motion =
		"@media (prefers-reduced-motion: reduce){:root{--motion-breath:0s;--motion-quick:0s}}";
	return [
		`:root{${declarations(scale)}}`,
		...themeBlocks.map(themeCss),
		motion,
	].join("");
}

export function installTokens(doc: Document): void {
	if (doc.getElementById("dalgona-tokens") !== null) return;
	const style = doc.createElement("style");
	style.id = "dalgona-tokens";
	style.textContent = tokenCss();
	doc.head.append(style);
}

export function applyTheme(
	setting: ThemeSetting,
	doc: Document,
	storage: Storage,
): void {
	const prefersDark =
		doc.defaultView?.matchMedia("(prefers-color-scheme: dark)").matches ===
		true;
	const resolved = resolveTheme(setting, prefersDark);
	doc.documentElement.setAttribute("data-theme", setting);
	storage.setItem(themeStorageKey, setting);
	let meta = doc.querySelector<HTMLMetaElement>('meta[name="theme-color"]');
	if (meta === null) {
		meta = doc.createElement("meta");
		meta.name = "theme-color";
		doc.head.append(meta);
	}
	meta.content = palette[resolved].canvas;
}
