export type RequestId = string | number;

export type ConnectionState =
	| "idle"
	| "connecting"
	| "connected"
	| "disconnected";

export type TurnStatus = "completed" | "interrupted" | "failed" | "inProgress";

export type ThreadActivity =
	| "notLoaded"
	| "idle"
	| "systemError"
	| "active"
	| "unknown";

export interface ThreadSummary {
	readonly id: string;
	readonly title: string;
	readonly preview: string;
	readonly status: ThreadActivity;
	readonly activeFlags?: readonly string[];
	readonly updatedAt: number;
	readonly model: string | null;
	readonly cwd: string;
}

export interface RetryWait {
	readonly threadId: string;
	readonly turnId: string;
	readonly remainingMs: number;
}

export interface FileChange {
	readonly path: string;
	readonly diff: string;
	readonly kind: string;
	readonly additions?: number;
	readonly deletions?: number;
	readonly diffOmitted: boolean;
}

export type JsonValue =
	| null
	| boolean
	| number
	| string
	| readonly JsonValue[]
	| { readonly [key: string]: JsonValue };

export interface EditReview {
	readonly verdict: "clean" | "uncertain" | "flagged" | "error";
	readonly summary: string;
}

export type TranscriptItem =
	| {
			readonly id: string;
			readonly kind: "userMessage";
			readonly text: string;
			readonly turnId: string;
			readonly keywords: readonly KeywordMatch[];
	  }
	| {
			readonly id: string;
			readonly kind: "agentMessage" | "reasoning";
			readonly text: string;
			readonly complete: boolean;
	  }
	| {
			readonly id: string;
			readonly kind: "commandExecution";
			readonly command: string;
			readonly output: string;
			readonly status: string;
			readonly exitCode: number | null;
			readonly durationMs: number | null;
	  }
	| {
			readonly id: string;
			readonly kind: "fileChange";
			readonly changes: readonly FileChange[];
			readonly status: string;
			readonly review: EditReview | null;
	  }
	| {
			readonly id: string;
			readonly kind: "toolCall";
			readonly name: string;
			readonly arguments: JsonValue;
			readonly output: string;
			readonly status: string;
			readonly durationMs: number | null;
	  }
	| { readonly id: string; readonly kind: "plan"; readonly text: string }
	| {
			readonly id: string;
			readonly kind: "advisorNote";
			readonly advisor: string;
			readonly severity: string;
			readonly note: string;
	  }
	| { readonly id: string; readonly kind: "contextCompaction" }
	| {
			readonly id: string;
			readonly kind: "unsupported";
			readonly label: string;
	  };

export interface TurnSnapshot {
	readonly id: string;
	readonly status: TurnStatus;
	readonly items: readonly TranscriptItem[];
	readonly error: string | null;
	readonly startedAtMs?: number | null;
}

export interface ThreadDetail {
	readonly summary: ThreadSummary;
	readonly turns: readonly TurnSnapshot[];
}

export type PlanStepStatus = "pending" | "inProgress" | "completed";

export interface PlanStep {
	readonly step: string;
	readonly status: PlanStepStatus;
}

export interface TurnPlan {
	readonly turnId: string;
	readonly explanation: string | null;
	readonly steps: readonly PlanStep[];
}

export interface TranscriptState {
	readonly threadId: string | null;
	readonly turnId: string | null;
	readonly turnStatus: TurnStatus | null;
	readonly items: readonly TranscriptItem[];
	readonly error: string | null;
	readonly plan: TurnPlan | null;
	readonly childThreadIds: readonly string[];
	readonly turnStartedAtMs: number | null;
	readonly turnReview: EditReview | null;
}

export type AdvisorStatus = "idle" | "reviewing" | "halted" | "disabled";

export interface AdvisorState {
	readonly name: string;
	readonly model: string;
	readonly status: AdvisorStatus;
	readonly notes: number;
}

export type AdvisorSummary =
	| { readonly kind: "loaded"; readonly advisors: readonly AdvisorState[] }
	| { readonly kind: "off" | "unavailable" | "unknown" };

export type AppNotification =
	| { readonly type: "threadStarted"; readonly thread: ThreadDetail }
	| {
			readonly type: "threadStatus";
			readonly threadId: string;
			readonly seq: number;
			readonly status: ThreadActivity;
			readonly activeFlags: readonly string[];
	  }
	| {
			readonly type: "threadName";
			readonly threadId: string;
			readonly name: string;
	  }
	| {
			readonly type: "turnStarted" | "turnCompleted";
			readonly threadId: string;
			readonly turn: TurnSnapshot;
	  }
	| {
			readonly type: "retryWait";
			readonly threadId: string;
			readonly turnId: string;
			readonly remainingMs: number;
	  }
	| {
			readonly type: "itemStarted" | "itemCompleted";
			readonly threadId: string;
			readonly turnId: string;
			readonly item: TranscriptItem;
	  }
	| {
			readonly type: "delta";
			readonly channel: "agent" | "reasoning" | "command";
			readonly threadId: string;
			readonly turnId: string;
			readonly itemId: string;
			readonly delta: string;
	  }
	| {
			readonly type: "error";
			readonly threadId: string;
			readonly turnId: string;
			readonly message: string;
			readonly willRetry: boolean;
	  }
	| {
			readonly type: "plan";
			readonly threadId: string;
			readonly turnId: string;
			readonly explanation: string | null;
			readonly steps: readonly PlanStep[];
	  }
	| { readonly type: "advisorNote"; readonly threadId: string }
	| {
			readonly type: "childSpawned";
			readonly threadId: string;
			readonly childId: string;
	  }
	| {
			readonly type: "toolJudged";
			readonly threadId: string;
			readonly itemId: string;
			readonly review: EditReview;
	  }
	| {
			readonly type: "turnJudged";
			readonly threadId: string;
			readonly turnId: string;
			readonly review: EditReview;
	  }
	| { readonly type: "requestResolved"; readonly requestId: RequestId };

export interface ModelOption {
	readonly id: string;
	readonly label: string;
	readonly description: string;
	readonly hidden: boolean;
}

export interface StatusInfo {
	readonly model: string;
	readonly approval: string;
	readonly sandbox: string;
	readonly runner: string;
}

export interface ThreadListResult {
	readonly threads: readonly ThreadSummary[];
	readonly nextCursor: string | null;
}

export interface ThreadStartResult {
	readonly thread: ThreadDetail;
	readonly status: StatusInfo;
}

export interface TurnStartResult {
	readonly turn: TurnSnapshot;
}

export interface TurnSteerResult {
	readonly turnId: string;
}

export interface InitializeInfo {
	readonly userAgent: string;
	readonly platformOs: string;
}

export interface KeywordDefinition {
	readonly word: string;
}

export interface KeywordMatch {
	readonly word: string;
	readonly byteRange: { readonly start: number; readonly end: number };
}

export interface ApprovalRequest {
	readonly id: RequestId;
	readonly method:
		| "item/commandExecution/requestApproval"
		| "item/fileChange/requestApproval";
	readonly threadId: string;
	readonly turnId: string;
	readonly itemId: string;
	readonly title: string;
	readonly detail: string;
	readonly reason: string | null;
}

export interface RpcHandlers {
	readonly onNotification: (notification: AppNotification) => void;
	readonly onRequest: (request: ApprovalRequest) => void;
	readonly onStateChange: (state: ConnectionState) => void;
	readonly onError: (message: string) => void;
}

export type NoticeTone = "error" | "warning" | "info";

export interface NoticeAction {
	readonly label: string;
	readonly run: () => void;
}

export interface Notice {
	readonly id: number;
	readonly tone: NoticeTone;
	readonly text: string;
	readonly actions: readonly NoticeAction[];
}

export interface WaitingElsewhere {
	readonly count: number;
	readonly threadId: string | null;
}
