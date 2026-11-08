import adapter from "@sveltejs/adapter-static";
import { sveltekit } from "@sveltejs/kit/vite";
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
	],
	test: {
		include: ["src/lib/**/*.test.ts"],
	},
});

export default config;
