import type { TranscriptItem } from "#lib/types.ts";

export interface RowProps {
	readonly item: TranscriptItem;
	readonly running: boolean;
	readonly awaiting: ReadonlySet<string>;
}
