import type { TranscriptItem } from "#lib/types.ts";
import type { RowPresentation } from "#lib/verbosity.ts";

export interface RowProps {
	readonly item: TranscriptItem;
	readonly running: boolean;
	readonly awaiting: ReadonlySet<string>;
	readonly presentation?: RowPresentation;
	readonly onOpen?: () => void;
}
