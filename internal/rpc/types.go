package rpc

import "encoding/json/jsontext"

type (
	ThreadCompactStartParams struct {
		ThreadID string `json:"threadId"`
	}
	ModelListParams            struct{}
	ConfigReadParams           struct{}
	AccountReadParams          struct{}
	DalgonaKeywordsListParams  struct{}
	DalgonaAdvisorStatusParams struct {
		ThreadID string `json:"threadId"`
	}
	DalgonaAdvisorSetParams struct {
		ThreadID string `json:"threadId"`
		Enabled  bool   `json:"enabled"`
	}

	ThreadStatus struct {
		Type        string   `json:"type"`
		ActiveFlags []string `json:"activeFlags,omitzero"`
	}
	Thread struct {
		ID             string       `json:"id"`
		SessionID      string       `json:"sessionId"`
		Name           string       `json:"name,omitzero"`
		Preview        string       `json:"preview"`
		Model          string       `json:"model,omitzero"`
		ModelProvider  string       `json:"modelProvider"`
		CreatedAt      int64        `json:"createdAt"`
		UpdatedAt      int64        `json:"updatedAt"`
		Cwd            string       `json:"cwd"`
		Ephemeral      bool         `json:"ephemeral"`
		CLIVersion     string       `json:"cliVersion"`
		Source         string       `json:"source"`
		Status         ThreadStatus `json:"status"`
		ParentThreadID string       `json:"parentThreadId,omitzero"`
		ForkedFromID   string       `json:"forkedFromId,omitzero"`
		Turns          []Turn       `json:"turns"`
	}
	Turn struct {
		ID          string     `json:"id"`
		Items       []Item     `json:"items"`
		Status      string     `json:"status"`
		Error       *TurnError `json:"error,omitzero"`
		StartedAt   int64      `json:"startedAt,omitzero"`
		CompletedAt int64      `json:"completedAt,omitzero"`
		DurationMs  int64      `json:"durationMs,omitzero"`
	}
	TurnError struct {
		Message string `json:"message"`
	}
	Item struct {
		Type       string
		ID         string
		Text       string
		Command    string
		Cwd        string
		Status     string
		Output     string
		ExitCode   *int
		Changes    []FileChange
		Server     string
		Tool       string
		Arguments  jsontext.Value
		DurationMs *int64
	}
	FileChange struct {
		Path string `json:"path"`
		Kind string `json:"kind"`
		Diff string `json:"diff"`
	}
	SandboxPolicy struct {
		Type string `json:"type"`
	}
	ThreadStartResponse struct {
		ApprovalPolicy    string        `json:"approvalPolicy"`
		ApprovalsReviewer string        `json:"approvalsReviewer"`
		Cwd               string        `json:"cwd"`
		Model             string        `json:"model"`
		ModelProvider     string        `json:"modelProvider"`
		Sandbox           SandboxPolicy `json:"sandbox"`
		ReasoningEffort   string        `json:"reasoningEffort,omitzero"`
		Thread            Thread        `json:"thread"`
	}
	ThreadResumeResponse = ThreadStartResponse
	ThreadForkResponse   = ThreadStartResponse
	ThreadListResponse   struct {
		Data       []Thread `json:"data"`
		NextCursor string   `json:"nextCursor,omitzero"`
	}
	ThreadReadResponse struct {
		Thread Thread `json:"thread"`
	}
	ThreadRevertResponse    = ThreadReadResponse
	ThreadUnarchiveResponse = ThreadReadResponse
	TurnStartResponse       struct {
		Turn Turn `json:"turn"`
	}
	TurnSteerResponse struct {
		TurnID string `json:"turnId"`
	}
	ModelListResponse struct {
		Data       []Model `json:"data"`
		NextCursor string  `json:"nextCursor,omitzero"`
	}
	Model struct {
		ID                        string                  `json:"id"`
		Model                     string                  `json:"model"`
		DisplayName               string                  `json:"displayName"`
		Description               string                  `json:"description"`
		Hidden                    bool                    `json:"hidden"`
		IsDefault                 bool                    `json:"isDefault"`
		DefaultReasoningEffort    string                  `json:"defaultReasoningEffort"`
		SupportedReasoningEfforts []ReasoningEffortOption `json:"supportedReasoningEfforts"`
	}
	ReasoningEffortOption struct {
		ReasoningEffort string `json:"reasoningEffort"`
		Description     string `json:"description"`
	}
	ConfigValues struct {
		Model                string `json:"model,omitzero"`
		ModelProvider        string `json:"model_provider,omitzero"`
		ApprovalPolicy       string `json:"approval_policy,omitzero"`
		SandboxMode          string `json:"sandbox_mode,omitzero"`
		ModelReasoningEffort string `json:"model_reasoning_effort,omitzero"`
	}
	ConfigReadResponse struct {
		Config  ConfigValues      `json:"config"`
		Origins map[string]string `json:"origins"`
	}
	ConfigWriteResponse struct {
		FilePath string `json:"filePath"`
		Status   string `json:"status"`
		Version  string `json:"version"`
	}
	Account struct {
		Type     string `json:"type"`
		Email    string `json:"email,omitzero"`
		PlanType string `json:"planType,omitzero"`
	}
	AccountReadResponse struct {
		Account            *Account `json:"account"`
		RequiresOpenaiAuth bool     `json:"requiresOpenaiAuth"`
	}
	LoginAccountResponse struct {
		Type            string `json:"type"`
		LoginID         string `json:"loginId,omitzero"`
		AuthURL         string `json:"authUrl,omitzero"`
		VerificationURL string `json:"verificationUrl,omitzero"`
		UserCode        string `json:"userCode,omitzero"`
	}
	CancelLoginAccountResponse struct {
		Status string `json:"status"`
	}
	SkillsListResponse struct {
		Data []SkillsListEntry `json:"data"`
	}
	SkillsListEntry struct {
		Cwd    string          `json:"cwd"`
		Skills []SkillMetadata `json:"skills"`
		Errors []string        `json:"errors"`
	}
	SkillMetadata struct {
		Name        string `json:"name"`
		Description string `json:"description"`
		Path        string `json:"path"`
		Scope       string `json:"scope"`
		Enabled     bool   `json:"enabled"`
	}
	Keyword struct {
		Word string `json:"word"`
		Hue  [2]int `json:"hue"`
	}
	ThreadTreeResponse struct {
		Turns []TreeTurn `json:"turns"`
	}
	TreeTurn struct {
		TurnID       string `json:"turnId"`
		ParentTurnID string `json:"parentTurnId"`
		Preview      string `json:"preview"`
		Active       bool   `json:"active"`
	}
	AdvisorState struct {
		Name   string       `json:"name"`
		Model  string       `json:"model"`
		Status string       `json:"status"`
		Notes  int          `json:"notes"`
		Usage  AdvisorUsage `json:"usage"`
	}
	AdvisorUsage struct {
		InputTokens  int `json:"inputTokens"`
		OutputTokens int `json:"outputTokens"`
	}

	RPCError struct {
		Code    int            `json:"code"`
		Message string         `json:"message"`
		Data    jsontext.Value `json:"data,omitzero"`
	}
	ErrorNotification struct {
		ThreadID  string    `json:"threadId"`
		TurnID    string    `json:"turnId"`
		WillRetry bool      `json:"willRetry"`
		Error     TurnError `json:"error"`
	}
	ThreadStartedNotification struct {
		Thread Thread `json:"thread"`
	}
	ThreadStatusChangedNotification struct {
		ThreadID string       `json:"threadId"`
		Status   ThreadStatus `json:"status"`
		Seq      uint64       `json:"seq"`
	}
	ThreadNameUpdatedNotification struct {
		ThreadID   string `json:"threadId"`
		ThreadName string `json:"threadName,omitzero"`
	}
	ThreadTokenUsageUpdatedNotification struct {
		ThreadID   string           `json:"threadId"`
		TurnID     string           `json:"turnId"`
		TokenUsage ThreadTokenUsage `json:"tokenUsage"`
	}
	ThreadTokenUsage struct {
		Last               TokenUsageBreakdown `json:"last"`
		Total              TokenUsageBreakdown `json:"total"`
		ModelContextWindow int                 `json:"modelContextWindow,omitzero"`
	}
	TokenUsageBreakdown struct {
		InputTokens           int `json:"inputTokens"`
		CachedInputTokens     int `json:"cachedInputTokens"`
		CacheWriteInputTokens int `json:"cacheWriteInputTokens"`
		OutputTokens          int `json:"outputTokens"`
		ReasoningOutputTokens int `json:"reasoningOutputTokens"`
		TotalTokens           int `json:"totalTokens"`
	}
	ThreadRevertedNotification struct {
		ThreadID string `json:"threadId"`
	}
	ThreadArchivedNotification   = ThreadRevertedNotification
	ThreadUnarchivedNotification = ThreadRevertedNotification
	TurnStartedNotification      struct {
		ThreadID string `json:"threadId"`
		Turn     Turn   `json:"turn"`
	}
	TurnCompletedNotification   = TurnStartedNotification
	TurnPlanUpdatedNotification struct {
		ThreadID string         `json:"threadId"`
		TurnID   string         `json:"turnId"`
		Plan     []TurnPlanStep `json:"plan"`
	}
	TurnPlanStep struct {
		Step   string `json:"step"`
		Status string `json:"status"`
	}
	ItemStartedNotification struct {
		ThreadID    string `json:"threadId"`
		TurnID      string `json:"turnId"`
		StartedAtMs int64  `json:"startedAtMs"`
		Item        Item   `json:"item"`
	}
	ItemCompletedNotification struct {
		ThreadID      string `json:"threadId"`
		TurnID        string `json:"turnId"`
		CompletedAtMs int64  `json:"completedAtMs"`
		Item          Item   `json:"item"`
	}
	AgentMessageDeltaNotification struct {
		ThreadID string `json:"threadId"`
		TurnID   string `json:"turnId"`
		ItemID   string `json:"itemId"`
		Delta    string `json:"delta"`
	}
	ReasoningTextDeltaNotification struct {
		ThreadID     string `json:"threadId"`
		TurnID       string `json:"turnId"`
		ItemID       string `json:"itemId"`
		Delta        string `json:"delta"`
		ContentIndex int    `json:"contentIndex"`
	}
	CommandExecutionOutputDeltaNotification = AgentMessageDeltaNotification
	ServerRequestResolvedNotification       struct {
		ThreadID  string `json:"threadId"`
		RequestID string `json:"requestId"`
	}
	AccountLoginCompletedNotification struct {
		LoginID string `json:"loginId,omitzero"`
		Success bool   `json:"success"`
		Error   string `json:"error,omitzero"`
	}
	ChildSpawnedNotification struct {
		ThreadID string `json:"threadId"`
		ChildID  string `json:"childId"`
		Cwd      string `json:"cwd"`
	}
	AdvisorNoteNotification struct {
		ThreadID string `json:"threadId"`
		Advisor  string `json:"advisor"`
		Severity string `json:"severity"`
		Note     string `json:"note"`
		Delivery string `json:"delivery"`
	}
	RetryWaitNotification struct {
		ThreadID    string `json:"threadId"`
		TurnID      string `json:"turnId"`
		RemainingMS int64  `json:"remainingMs"`
	}
	CommandExecutionRequestApprovalParams struct {
		ThreadID    string `json:"threadId"`
		TurnID      string `json:"turnId"`
		ItemID      string `json:"itemId"`
		StartedAtMs int64  `json:"startedAtMs"`
		Command     string `json:"command,omitzero"`
		Cwd         string `json:"cwd,omitzero"`
		Reason      string `json:"reason,omitzero"`
	}
	FileChangeRequestApprovalParams struct {
		ThreadID    string `json:"threadId"`
		TurnID      string `json:"turnId"`
		ItemID      string `json:"itemId"`
		StartedAtMs int64  `json:"startedAtMs"`
		Reason      string `json:"reason,omitzero"`
	}
	CommandExecutionRequestApprovalResponse struct {
		Decision string `json:"decision"`
	}
	FileChangeRequestApprovalResponse = CommandExecutionRequestApprovalResponse
)

func (e *RPCError) Error() string { return e.Message }
