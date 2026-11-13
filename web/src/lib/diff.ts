export interface DiffLine {
	readonly kind: "add" | "del" | "hunk" | "meta" | "context";
	readonly text: string;
}

export function parseDiff(diff: string): readonly DiffLine[] {
	const lines = diff.split("\n");
	if (lines[lines.length - 1] === "") {
		lines.pop();
	}
	const parsed: DiffLine[] = [];
	let inBody = false;
	for (const text of lines) {
		if (text.startsWith("@@")) {
			inBody = true;
			parsed.push({ kind: "hunk", text });
		} else if (!inBody) {
			const meta = text.startsWith("---") || text.startsWith("+++");
			parsed.push({ kind: meta ? "meta" : "context", text });
		} else if (text.startsWith("+")) {
			parsed.push({ kind: "add", text });
		} else if (text.startsWith("-")) {
			parsed.push({ kind: "del", text });
		} else {
			parsed.push({ kind: "context", text });
		}
	}
	return parsed;
}
