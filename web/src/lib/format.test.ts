import { describe, expect, test } from "vitest";
import { formatDuration } from "./format";

describe("formatDuration", () => {
	test("uses one form at each boundary", () => {
		expect(formatDuration(0)).toBe("0 ms");
		expect(formatDuration(999)).toBe("999 ms");
		expect(formatDuration(1000)).toBe("1s");
		expect(formatDuration(59999)).toBe("59s");
		expect(formatDuration(60000)).toBe("1m 00s");
		expect(formatDuration(3599999)).toBe("59m 59s");
		expect(formatDuration(3600000)).toBe("1h 00m");
	});
});
