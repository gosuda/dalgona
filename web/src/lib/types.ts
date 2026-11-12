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

export interface OutputSpill {
	readonly path: string;
	readonly bytes: number;
	readonly sha256: string;
}

export interface OutputPage {
	readonly text: string;
	readonly offset: number;
	readonly next: number;
	readonly total: number;
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
			readonly spill?: OutputSpill;
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
			readonly spill?: OutputSpill;
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

export interface TreeTurn {
	readonly turnId: string;
	readonly parentTurnId: string | null;
	readonly preview: string;
	readonly active: boolean;
}

export type ReviewTarget =
	| { readonly type: "uncommittedChanges" }
	| { readonly type: "baseBranch"; readonly branch: string }
	| { readonly type: "commit"; readonly sha: string }
	| { readonly type: "custom"; readonly instructions: string };

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
	| {
			readonly type: "tokenUsage";
			readonly threadId: string;
			readonly turnId: string;
			readonly usage: TokenUsage;
	  }
	| {
			readonly type: "fileChangesUpdated";
			readonly threadId: string;
			readonly itemId: string;
			readonly changes: readonly FileChange[];
	  }
	| {
			readonly type: "requestResolved";
			readonly requestId: RequestId;
			readonly resolution: string | null;
	  }
	| {
			readonly type: "loginCompleted";
			readonly loginId: string | null;
			readonly success: boolean;
			readonly error: string | null;
			readonly provider: string | null;
	  };

export type LoginMethod = "apiKey" | "browser" | "device";

export interface LoginProvider {
	readonly id: string;
	readonly login: "apiKey" | "browser" | "terminal";
	readonly signedIn: boolean;
}

export type LoginStart =
	| { readonly type: "apiKey" }
	| {
			readonly type: "chatgpt";
			readonly loginId: string;
			readonly authUrl: string;
	  }
	| {
			readonly type: "chatgptDeviceCode";
			readonly loginId: string;
			readonly verificationUrl: string;
			readonly userCode: string;
	  };

export interface LoginFlow {
	readonly provider: string;
	readonly method: LoginMethod;
	readonly loginId: string | null;
	readonly authUrl: string | null;
	readonly verificationUrl: string | null;
	readonly userCode: string | null;
	readonly error: string | null;
	readonly pending: boolean;
}

export interface ModelOption {
	readonly id: string;
	readonly label: string;
	readonly description: string;
	readonly hidden: boolean;
	readonly efforts: readonly string[];
	readonly defaultEffort: string | null;
	readonly presets: readonly ModelPreset[];
}

export type ModelPreset = "default" | "smol" | "slow";

export interface StatusInfo {
	readonly model: string;
	readonly approval: string;
	readonly sandbox: string;
	readonly effort: string | null;
}

export interface TokenBreakdown {
	readonly totalTokens: number;
	readonly inputTokens: number;
	readonly cachedInputTokens: number;
	readonly cacheWriteInputTokens: number;
	readonly outputTokens: number;
	readonly reasoningOutputTokens: number;
}

export interface TokenUsage {
	readonly last: TokenBreakdown;
	readonly total: TokenBreakdown;
	readonly contextWindow: number | null;
}

export interface ThreadListResult {
	readonly threads: readonly ThreadSummary[];
	readonly nextCursor: string | null;
}

export interface ThreadStartResult {
	readonly thread: ThreadDetail;
	readonly status: StatusInfo;
	readonly usage: TokenUsage | null;
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

export interface FileMatch {
	readonly fileName: string;
	readonly path: string;
	readonly root: string;
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

export interface QuestionOption {
	readonly label: string;
	readonly description: string;
	readonly preview: string | null;
}

export interface QuestionItem {
	readonly id: string;
	readonly header: string;
	readonly question: string;
	readonly options: readonly QuestionOption[];
	readonly multiSelect: boolean;
	readonly isOther: boolean;
}

export interface QuestionRequest {
	readonly id: RequestId;
	readonly method: "item/tool/requestUserInput";
	readonly threadId: string;
	readonly turnId: string;
	readonly itemId: string;
	readonly questions: readonly QuestionItem[];
}

export interface QuestionSelection {
	readonly picked: readonly string[];
	readonly other: string;
}

export type ServerRequest = ApprovalRequest | QuestionRequest;

export interface RpcHandlers {
	readonly onNotification: (notification: AppNotification) => void;
	readonly onRequest: (request: ApprovalRequest) => void;
	readonly onQuestion: (request: QuestionRequest) => void;
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
