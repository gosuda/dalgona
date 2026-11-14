const colorRoles = [
	"canvas",
	"surface",
	"tone",
	"line",
	"strong",
	"text",
	"dim",
	"faint",
	"accentText",
	"failedText",
	"addedText",
	"warningText",
] as const;

export type ColorRole = (typeof colorRoles)[number];

export const concreteThemes = [
	"day",
	"soft-day",
	"night",
	"soft-night",
] as const;

export type ConcreteTheme = (typeof concreteThemes)[number];

export type Hex = `#${string}`;

export const palette: Readonly<
	Record<ConcreteTheme, Readonly<Record<ColorRole, Hex>>>
> = {
	night: {
		canvas: "#111113",
		surface: "#18191b",
		tone: "#212225",
		line: "#363a3f",
		strong: "#696e77",
		text: "#CECDC3",
		dim: "#9F9D96",
		faint: "#6F6E69",
		accentText: "#DA702C",
		failedText: "#E8705F",
		addedText: "#879A39",
		warningText: "#D0A215",
	},
	"soft-night": {
		canvas: "#212225",
		surface: "#272a2d",
		tone: "#2e3135",
		line: "#43484e",
		strong: "#777b84",
		text: "#CECDC3",
		dim: "#9F9D96",
		faint: "#6F6E69",
		accentText: "#EC8B49",
		failedText: "#F89A8A",
		addedText: "#A0AF54",
		warningText: "#D0A215",
	},
	day: {
		canvas: "#FFFCF0",
		surface: "#E6E4D9",
		tone: "#CECDC3",
		line: "#B7B5AC",
		strong: "#6F6E69",
		text: "#100F0F",
		dim: "#403E3C",
		faint: "#6F6E69",
		accentText: "#71320D",
		failedText: "#6C201C",
		addedText: "#3D4C07",
		warningText: "#664D01",
	},
	"soft-day": {
		canvas: "#E6E4D9",
		surface: "#FFFCF0",
		tone: "#CECDC3",
		line: "#B7B5AC",
		strong: "#6F6E69",
		text: "#100F0F",
		dim: "#403E3C",
		faint: "#6F6E69",
		accentText: "#71320D",
		failedText: "#6C201C",
		addedText: "#3D4C07",
		warningText: "#664D01",
	},
};

export type HuePair = "night" | "day";

interface HueColours {
	readonly point: Hex;
	readonly onPoint: Hex;
}

export interface Hue {
	readonly name: string;
	readonly night: HueColours;
	readonly day: HueColours;
}

export const hues: readonly Hue[] = [
	{
		name: "orange",
		night: { point: "#EC8B49", onPoint: "#100F0F" },
		day: { point: "#9D4310", onPoint: "#FFFCF0" },
	},
	{
		name: "cyan",
		night: { point: "#3AA99F", onPoint: "#100F0F" },
		day: { point: "#1C6C66", onPoint: "#FFFCF0" },
	},
	{
		name: "blue",
		night: { point: "#66A0C8", onPoint: "#100F0F" },
		day: { point: "#1A4F8C", onPoint: "#FFFCF0" },
	},
	{
		name: "magenta",
		night: { point: "#E47DA8", onPoint: "#100F0F" },
		day: { point: "#87285E", onPoint: "#FFFCF0" },
	},
];

export function huePair(theme: ConcreteTheme): HuePair {
	return theme === "night" || theme === "soft-night" ? "night" : "day";
}

interface ContrastRule {
	readonly fg: ColorRole;
	readonly bg: ColorRole;
	readonly min: number;
}

const textRoles = [
	"text",
	"dim",
	"accentText",
	"failedText",
	"addedText",
	"warningText",
] as const;
const groundRoles = ["canvas", "surface", "tone"] as const;

export const contrastRules: readonly ContrastRule[] = [
	...textRoles.flatMap((fg) => groundRoles.map((bg) => ({ fg, bg, min: 4.5 }))),
	...groundRoles.map((bg) => ({ fg: "strong" as const, bg, min: 3 })),
];
