import { describe, expect, it } from "vitest";
import { rpcSocketUrl, tokenFromHash } from "./protocol.js";

describe("tokenFromHash", () => {
	it("reads the token from the URL fragment", () => {
		expect(tokenFromHash("#token=abc123")).toBe("abc123");
	});

	it("returns an empty string when no token is present", () => {
		expect(tokenFromHash("")).toBe("");
		expect(tokenFromHash("#foo=bar")).toBe("");
	});
});

describe("rpcSocketUrl", () => {
	it("builds the websocket URL with an encoded token", () => {
		expect(rpcSocketUrl("example.com", true, "a b")).toBe(
			"wss://example.com/rpc?token=a%20b",
		);
		expect(rpcSocketUrl("example.com:8080", false, "abc")).toBe(
			"ws://example.com:8080/rpc?token=abc",
		);
	});
});
