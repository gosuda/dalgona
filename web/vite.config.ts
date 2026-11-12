import adapter from "@sveltejs/adapter-static";
import { sveltekit } from "@sveltejs/kit/vite";
import { svelteTesting } from "@testing-library/svelte/vite";
import type { UserConfig } from "vite";
import { defineConfig } from "vitest/config";

const config: UserConfig = defineConfig({
	plugins: [
		sveltekit({
			adapter: adapter({
				pages: "../internal/webui/dist",
				assets: "../internal/webui/dist",
				fallback: "200.html",
			}),
			compilerOptions: {
				runes: true,
			},
		}),
		svelteTesting(),
	],
	test: {
		projects: [
			{
				extends: true,
				test: {
					name: "unit",
					environment: "node",
					include: ["src/lib/**/*.test.ts"],
					exclude: ["src/lib/**/*.dom.test.ts"],
				},
			},
			{
				extends: true,
				test: {
					name: "dom",
					environment: "happy-dom",
					setupFiles: ["src/dom-setup.ts"],
					css: {
						include: [
							/NoticeStack\.svelte/,
							/ApprovalDialog\.svelte/,
							/ThreadList\.svelte/,
							/app\.css$/,
						],
					},
					include: ["src/lib/**/*.dom.test.ts"],
				},
			},
		],
	},
});

export default config;
