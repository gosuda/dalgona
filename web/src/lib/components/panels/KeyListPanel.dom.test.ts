import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import App from "#lib/components/App.svelte";
import KeyListPanel from "#lib/components/panels/KeyListPanel.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type { RpcHandlers } from "#lib/types.ts";

afterEach(() => {
	cleanup();
	document.body.replaceChildren();
});

function idlePort(): RpcPort {
	return {
		call: () => Promise.reject(new Error("no call in this test")),
		notify: (): void => undefined,
		respond: (): void => undefined,
		open: () => Promise.resolve(),
		close: (): void => undefined,
	};
}

describe("key list panel", () => {
	test("lists the composer, transcript and request keys", () => {
		render(KeyListPanel, { props: { open: true, onClose: vi.fn() } });
		expect(screen.getByRole("heading", { name: "Composer" })).toBeTruthy();
		expect(screen.getByText("Alt+Enter")).toBeTruthy();
		expect(screen.getByText("Ctrl+O")).toBeTruthy();
		expect(screen.getByText("Accept for session")).toBeTruthy();
	});

	test("? opens the key list and Esc returns focus", async () => {
		const dial = (_url: string, _handlers: RpcHandlers): RpcPort => idlePort();
		const session = new Session(dial);
		render(App, { props: { session } });
		const opener = document.createElement("button");
		document.body.append(opener);
		opener.focus();

		await fireEvent.keyDown(opener, { key: "?" });
		expect(session.panel).toBe("keys");
		await tick();
		const dialog = document.querySelector<HTMLElement>("dialog[open]");
		expect(dialog?.textContent).toContain("Composer");

		await fireEvent.keyDown(dialog as HTMLElement, { key: "Escape" });
		expect(session.panel).toBeNull();
		expect(document.querySelector("dialog[open]")).toBeNull();
		expect(document.activeElement).toBe(opener);
	});
});
