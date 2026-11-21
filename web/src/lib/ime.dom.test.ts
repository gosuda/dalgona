import { waitFor } from "@testing-library/dom";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { createRawSnippet, tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import {
	MAX_MESSAGE_BYTES,
	messageBytes,
	NO_ATTACHMENTS,
} from "#lib/attachments.ts";
import ApprovalCard from "#lib/components/ApprovalCard.svelte";
import CommandPalette from "#lib/components/CommandPalette.svelte";
import Composer from "#lib/components/Composer.svelte";
import KeywordText from "#lib/components/KeywordText.svelte";
import Panel from "#lib/components/panels/Panel.svelte";
import QuestionCard from "#lib/components/QuestionCard.svelte";
import type {
	ApprovalRequest,
	FileMatch,
	KeywordMatch,
	QuestionRequest,
	QuestionSelection,
} from "#lib/types.ts";

afterEach(cleanup);

function baseProps() {
	return {
		disabled: false,
		fileSearchRoot: "/work",
		running: false,
		followups: [] as readonly string[],
		history: [] as readonly string[],
		onEditFollowup: vi.fn(() => undefined),
		onRemoveFollowup: vi.fn(),
		onSend: vi.fn(),
		onSteer: vi.fn(),
		onFollowup: vi.fn(),
		onInterrupt: vi.fn(),
	};
}

function messageField(): HTMLTextAreaElement {
	return screen.getByRole("textbox", {
		name: "Message",
	}) as HTMLTextAreaElement;
}

function loneSurrogates(value: string): boolean {
	for (let index = 0; index < value.length; index += 1) {
		const code = value.charCodeAt(index);
		if (code >= 0xd800 && code <= 0xdbff) {
			const next = value.charCodeAt(index + 1);
			if (!(next >= 0xdc00 && next <= 0xdfff)) return true;
			index += 1;
		} else if (code >= 0xdc00 && code <= 0xdfff) {
			return true;
		}
	}
	return false;
}

function utf8Bytes(value: string): number {
	return new TextEncoder().encode(value).length;
}

const NO_ATTACHMENTS_SENT = { texts: [], images: [] };

const cjkRequest: QuestionRequest = {
	id: "req-ime-q",
	method: "item/tool/requestUserInput",
	threadId: "thread-1",
	turnId: "turn-1",
	itemId: "call-ime",
	questions: [
		{
			id: "q-ime",
			header: "自由記述",
			question: "日本語で答えてください",
			options: [],
			multiSelect: false,
			isOther: false,
		},
	],
};

const approvalRequest: ApprovalRequest = {
	id: "req-ime-a",
	method: "item/commandExecution/requestApproval",
	threadId: "thread-1",
	turnId: "turn-1",
	itemId: "run-ime",
	title: "Run ls",
	detail: "ls",
	reason: null,
};

describe("IME text input", () => {
	test("Chinese input sends intact and fullwidth marks stay literal", async () => {
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend } });
		const field = messageField();
		await fireEvent.input(field, { target: { value: "中文输入测试" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenCalledWith("中文输入测试", NO_ATTACHMENTS_SENT);
		await fireEvent.input(field, { target: { value: "／model" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenLastCalledWith("／model", NO_ATTACHMENTS_SENT);
		await fireEvent.input(field, { target: { value: "！ls" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenLastCalledWith("！ls", NO_ATTACHMENTS_SENT);
	});

	test("ideographic space keeps the mention token whole and sends it", async () => {
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend } });
		const field = messageField();
		const value = "@文件.ts　说明";
		await fireEvent.input(field, { target: { value } });
		field.setSelectionRange(value.length, value.length);
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenCalledWith(value, NO_ATTACHMENTS_SENT);
	});

	test("a command with an ideographic space reports no command", async () => {
		const onSend = vi.fn();
		const onOpenPanel = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend, onOpenPanel } });
		const field = messageField();
		await fireEvent.input(field, { target: { value: "/model　fast" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).not.toHaveBeenCalled();
		expect(onOpenPanel).not.toHaveBeenCalled();
		expect(screen.getByRole("alert").textContent).toContain(
			"No command named /model　fast",
		);
	});

	test("Korean Hangul and Jamo NFD send without normalization", async () => {
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend } });
		const field = messageField();
		await fireEvent.input(field, { target: { value: "한글 입력" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenLastCalledWith("한글 입력", NO_ATTACHMENTS_SENT);
		await fireEvent.input(field, { target: { value: "한글" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenLastCalledWith("한글", NO_ATTACHMENTS_SENT);
	});

	test("Japanese kana and kanji send intact", async () => {
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend } });
		const field = messageField();
		await fireEvent.input(field, {
			target: { value: "日本語入力テスト" },
		});
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenCalledWith(
			"日本語入力テスト",
			NO_ATTACHMENTS_SENT,
		);
		await fireEvent.input(field, { target: { value: "にほんご" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenLastCalledWith("にほんご", NO_ATTACHMENTS_SENT);
	});

	test("a Korean file mention searches the Hangul query", async () => {
		const onFileSearch = vi.fn(() =>
			Promise.resolve([
				{ fileName: "파일.ts", path: "src/파일.ts", root: "/work" },
			] satisfies readonly FileMatch[]),
		);
		render(Composer, { props: { ...baseProps(), onFileSearch } });
		const field = messageField();
		await fireEvent.input(field, { target: { value: "@파일.ts" } });
		await waitFor(() => {
			expect(onFileSearch).toHaveBeenCalledWith("파일.ts");
		});
	});
});

describe("emoji and UTF-16 handling", () => {
	test("ZWJ, skin tone, keycap and flag emoji send without lone surrogates", async () => {
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend } });
		const field = messageField();
		const text = "👨‍👩‍👧‍👦 👍🏽 1️⃣ 🇰🇷🇯🇵🇨🇳";
		await fireEvent.input(field, { target: { value: text } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenCalledWith(text, NO_ATTACHMENTS_SENT);
		const sent: unknown = onSend.mock.calls[0]?.[0];
		expect(typeof sent === "string" && loneSurrogates(sent)).toBe(false);
	});

	test("file mention insertion around flags keeps every code point", async () => {
		const onFileSearch = vi.fn(() =>
			Promise.resolve([
				{ fileName: "文件.ts", path: "src/文件.ts", root: "/work" },
			] satisfies readonly FileMatch[]),
		);
		render(Composer, { props: { ...baseProps(), onFileSearch } });
		const field = messageField();
		const prefix = "🇰🇷🇯🇵 ";
		await fireEvent.input(field, { target: { value: `${prefix}@文` } });
		await waitFor(() => {
			expect(
				screen.getByRole("listbox", { name: "Files" }).textContent,
			).toContain("src/文件.ts");
		});
		await fireEvent.keyDown(field, { key: "Enter" });
		const expected = `${prefix}@src/文件.ts `;
		await waitFor(() => {
			expect(field.value).toBe(expected);
		});
		expect(loneSurrogates(field.value)).toBe(false);
	});

	test("history recall preserves emoji messages across ArrowUp and ArrowDown", async () => {
		render(Composer, {
			props: {
				...baseProps(),
				history: ["🇰🇷🇯🇵 🇨🇳 first", "👨‍👩‍👧‍👦 last"],
			},
		});
		const field = messageField();
		field.setSelectionRange(0, 0);
		await fireEvent.keyDown(field, { key: "ArrowUp" });
		expect(field.value).toBe("👨‍👩‍👧‍👦 last");
		field.setSelectionRange(0, 0);
		await fireEvent.keyDown(field, { key: "ArrowUp" });
		expect(field.value).toBe("🇰🇷🇯🇵 🇨🇳 first");
		field.setSelectionRange(field.value.length, field.value.length);
		await fireEvent.keyDown(field, { key: "ArrowDown" });
		expect(field.value).toBe("👨‍👩‍👧‍👦 last");
		field.setSelectionRange(field.value.length, field.value.length);
		expect(field.selectionStart).toBe(field.value.length);
		await fireEvent.keyDown(field, { key: "ArrowDown" });
		expect(field.value).toBe("");
		expect(loneSurrogates(field.value)).toBe(false);
	});
});

describe("messageBytes UTF-8 boundary", () => {
	test("counts UTF-8 bytes, not UTF-16 units", () => {
		const ascii = messageBytes("x", NO_ATTACHMENTS);
		expect(messageBytes("中", NO_ATTACHMENTS)).toBe(ascii + 2);
		expect(messageBytes("👨", NO_ATTACHMENTS)).toBe(ascii + 3);
	});

	test("accepts MAX_MESSAGE_BYTES - 1, MAX_MESSAGE_BYTES and rejects + 1", () => {
		const probe = messageBytes("x", NO_ATTACHMENTS);
		const overhead = probe - utf8Bytes("x");
		const capacity = MAX_MESSAGE_BYTES - overhead;
		const filler = "中".repeat(Math.floor(capacity / 3));
		const rest = capacity - utf8Bytes(filler);
		const exact = filler + "a".repeat(rest);
		expect(messageBytes(exact, NO_ATTACHMENTS)).toBe(MAX_MESSAGE_BYTES);
		expect(messageBytes(`${exact}a`, NO_ATTACHMENTS)).toBe(
			MAX_MESSAGE_BYTES + 1,
		);
		const trimmed = exact.slice(0, -1);
		expect(messageBytes(trimmed, NO_ATTACHMENTS)).toBeLessThan(
			MAX_MESSAGE_BYTES,
		);
	});
});

describe("KeywordText byte ranges", () => {
	test("marks a keyword after CJK and emoji prefixes without U+FFFD", () => {
		const text = "前缀 👨‍👩‍👧‍👦 ultrathink 后缀";
		const needle = "ultrathink";
		const start = utf8Bytes(text.slice(0, text.indexOf(needle)));
		const match: KeywordMatch = {
			word: needle,
			byteRange: { start, end: start + utf8Bytes(needle) },
		};
		const { container } = render(KeywordText, {
			props: { text, matches: [match] },
		});
		expect(container.querySelector("mark.keyword")?.textContent).toBe(needle);
		expect(container.textContent).toBe(text);
		expect(container.textContent).not.toContain("�");
	});

	test("marks a span covering a ZWJ emoji sequence", () => {
		const text = "前缀 👨‍👩‍👧‍👦 后缀";
		const emoji = "👨‍👩‍👧‍👦";
		const start = utf8Bytes(text.slice(0, text.indexOf(emoji)));
		const match: KeywordMatch = {
			word: emoji,
			byteRange: { start, end: start + utf8Bytes(emoji) },
		};
		const { container } = render(KeywordText, {
			props: { text, matches: [match] },
		});
		expect(container.querySelector("mark.keyword")?.textContent).toBe(emoji);
		expect(container.textContent).toBe(text);
		expect(loneSurrogates(container.textContent ?? "")).toBe(false);
	});
});

describe("IME composition Escape", () => {
	test("Escape during composition does not interrupt a retrying turn", async () => {
		const onInterrupt = vi.fn();
		render(Composer, {
			props: {
				...baseProps(),
				running: true,
				retrying: true,
				onInterrupt,
			},
		});
		const field = messageField();
		const help = () => document.getElementById("composer-help")?.textContent;
		const before = help();
		await fireEvent.keyDown(field, { key: "Escape", isComposing: true });
		await fireEvent.keyDown(field, { key: "Escape", keyCode: 229 });
		await fireEvent.keyDown(field, { key: "Process" });
		expect(onInterrupt).not.toHaveBeenCalled();
		expect(help()).toBe(before);
	});

	test("Escape during composition does not arm the interrupt guard", async () => {
		const onInterrupt = vi.fn();
		render(Composer, {
			props: { ...baseProps(), running: true, onInterrupt },
		});
		const field = messageField();
		const help = () => document.getElementById("composer-help")?.textContent;
		const before = help();
		await fireEvent.keyDown(field, { key: "Escape", isComposing: true });
		await fireEvent.keyDown(field, { key: "Escape", keyCode: 229 });
		expect(onInterrupt).not.toHaveBeenCalled();
		expect(help()).toBe(before);
		await fireEvent.keyDown(field, { key: "Escape" });
		expect(onInterrupt).not.toHaveBeenCalled();
		expect(help()).toBe("Esc again to interrupt");
		await fireEvent.keyDown(field, { key: "Escape" });
		expect(onInterrupt).toHaveBeenCalledTimes(1);
	});

	test("Enter during composition does not steer, queue or send", async () => {
		const onSend = vi.fn();
		const onSteer = vi.fn();
		const onFollowup = vi.fn();
		render(Composer, {
			props: {
				...baseProps(),
				running: true,
				onSend,
				onSteer,
				onFollowup,
			},
		});
		const field = messageField();
		await fireEvent.input(field, { target: { value: "日本語の返答" } });
		await fireEvent.keyDown(field, { key: "Enter", isComposing: true });
		await fireEvent.keyDown(field, { key: "Enter", keyCode: 229 });
		await fireEvent.keyDown(field, {
			key: "Enter",
			altKey: true,
			isComposing: true,
		});
		await fireEvent.keyDown(field, { key: "Process" });
		expect(onSteer).not.toHaveBeenCalled();
		expect(onFollowup).not.toHaveBeenCalled();
		expect(onSend).not.toHaveBeenCalled();
		expect(field.value).toBe("日本語の返答");
	});

	test("Enter during composition does not send while idle", async () => {
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend } });
		const field = messageField();
		await fireEvent.input(field, { target: { value: "中文" } });
		await fireEvent.keyDown(field, { key: "Enter", isComposing: true });
		await fireEvent.keyDown(field, { key: "Enter", keyCode: 229 });
		await fireEvent.keyDown(field, { key: "Process" });
		expect(onSend).not.toHaveBeenCalled();
		expect(field.value).toBe("中文");
	});

	test("palette keys during composition move nothing and open nothing", async () => {
		const node = document.createElement("textarea");
		document.body.append(node);
		const onOpen = vi.fn();
		const onClose = vi.fn();
		render(CommandPalette, {
			props: {
				id: "palette-ime",
				field: node,
				query: "",
				active: 0,
				onOpen,
				onClose,
			},
		});
		await tick();
		const first = document.getElementById("palette-ime-0");
		expect(first?.getAttribute("aria-selected")).toBe("true");
		await fireEvent.keyDown(node, { key: "ArrowDown", isComposing: true });
		await fireEvent.keyDown(node, { key: "ArrowUp", keyCode: 229 });
		await fireEvent.keyDown(node, { key: "Tab", isComposing: true });
		await fireEvent.keyDown(node, { key: "Enter", isComposing: true });
		await fireEvent.keyDown(node, { key: "Escape", isComposing: true });
		await fireEvent.keyDown(node, { key: "Process" });
		expect(onOpen).not.toHaveBeenCalled();
		expect(onClose).not.toHaveBeenCalled();
		expect(first?.getAttribute("aria-selected")).toBe("true");
		await fireEvent.keyDown(node, { key: "ArrowDown" });
		await fireEvent.keyDown(node, { key: "Enter" });
		expect(onOpen).toHaveBeenCalledWith("settings");
	});

	test("panel ignores Escape and cancel while an IME composition runs", async () => {
		const onClose = vi.fn();
		const children = createRawSnippet(() => ({
			render: () => '<input id="ime-field" type="text">',
		}));
		const { container } = render(Panel, {
			props: { title: "Panel", open: true, onClose, children },
		});
		await tick();
		const dialog = container.querySelector("dialog");
		const input = container.querySelector<HTMLInputElement>("#ime-field");
		if (dialog === null || input === null) {
			throw new Error("panel did not render");
		}
		await fireEvent.input(input, { target: { value: "日本語入力" } });
		await fireEvent.keyDown(input, { key: "Escape", keyCode: 229 });
		expect(onClose).not.toHaveBeenCalled();
		await fireEvent.compositionStart(input);
		await fireEvent.keyDown(input, { key: "Escape" });
		expect(onClose).not.toHaveBeenCalled();
		dialog.dispatchEvent(new Event("cancel", { cancelable: true }));
		expect(onClose).not.toHaveBeenCalled();
		expect(input.value).toBe("日本語入力");
		await fireEvent.compositionEnd(input);
		await fireEvent.keyDown(input, { key: "Escape" });
		expect(onClose).toHaveBeenCalledTimes(1);
	});

	test("question card keeps CJK answers when Escape composes", async () => {
		const onSubmit = vi.fn();
		const onDismiss = vi.fn();
		render(QuestionCard, {
			props: {
				request: cjkRequest,
				position: 1,
				total: 1,
				subagent: null,
				onSubmit,
				onDismiss,
			},
		});
		const field = screen.getByRole("textbox", {
			name: "自由記述",
		}) as HTMLInputElement;
		await fireEvent.input(field, { target: { value: "日本語の回答" } });
		await fireEvent.keyDown(field, { key: "Escape", isComposing: true });
		await fireEvent.keyDown(field, { key: "Escape", keyCode: 229 });
		await fireEvent.keyDown(field, { key: "Process" });
		expect(onDismiss).not.toHaveBeenCalled();
		expect(field.value).toBe("日本語の回答");
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		const selections = onSubmit.mock.calls[0]?.[1] as
			| readonly QuestionSelection[]
			| undefined;
		expect(selections?.[0]?.other).toBe("日本語の回答");
	});

	test("question card still dismisses on a plain Escape", async () => {
		const onDismiss = vi.fn();
		render(QuestionCard, {
			props: {
				request: cjkRequest,
				position: 1,
				total: 1,
				subagent: null,
				onSubmit: vi.fn(),
				onDismiss,
			},
		});
		const field = screen.getByRole("textbox", {
			name: "自由記述",
		}) as HTMLInputElement;
		await fireEvent.keyDown(field, { key: "Escape" });
		expect(onDismiss).toHaveBeenCalledWith(cjkRequest);
	});

	test("approval card ignores candidate keys during composition", async () => {
		const onDecision = vi.fn();
		const { container } = render(ApprovalCard, {
			props: {
				request: approvalRequest,
				changes: [],
				position: 1,
				total: 1,
				subagent: null,
				onDecision,
			},
		});
		const card = container.querySelector("section");
		if (card === null) throw new Error("no approval card");
		await fireEvent.keyDown(card, { key: "n", isComposing: true });
		await fireEvent.keyDown(card, { key: "y", keyCode: 229 });
		await fireEvent.keyDown(card, { key: "Escape", isComposing: true });
		await fireEvent.keyDown(card, { key: "Process" });
		expect(onDecision).not.toHaveBeenCalled();
		await fireEvent.keyDown(card, { key: "n" });
		expect(onDecision).toHaveBeenCalledWith(approvalRequest, "decline");
	});
});
