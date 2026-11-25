//go:build gui

package manage

import "encoding/json/jsontext"

const (
	methodOperationRead     = "dalgona/operation/read"
	methodThreadList        = "thread/list"
	methodThreadName        = "thread/name/set"
	methodThreadArchive     = "thread/archive"
	methodThreadUnarchive   = "thread/unarchive"
	methodRemoteStatus      = "dalgona/remote/status"
	methodRemotePull        = "dalgona/remote/pull"
	methodRemoteGC          = "dalgona/remote/gc"
	methodRemoteProgress    = "dalgona/remote/progress"
	methodMCPConfigRead     = "dalgona/mcp/config/read"
	methodMCPConfigWrite    = "dalgona/mcp/config/write"
	methodMCPStatus         = "mcpServerStatus/list"
	methodAuthProviders     = "dalgona/auth/providers"
	methodAuthStart         = "dalgona/auth/start"
	methodAuthSubmit        = "dalgona/auth/submit"
	methodAuthStatus        = "dalgona/auth/status"
	methodAuthCancel        = "dalgona/auth/cancel"
	methodAuthLogout        = "dalgona/auth/logout"
	methodWorkspaceOpen     = "dalgona/workspace/open"
	methodWorkspaceClose    = "dalgona/workspace/close"
	methodWorkspaceList     = "dalgona/workspace/list"
	methodWorkspaceStat     = "dalgona/workspace/stat"
	methodWorkspaceRead     = "dalgona/workspace/read"
	methodWorkspaceWrite    = "dalgona/workspace/write"
	methodWorkspaceMkdir    = "dalgona/workspace/mkdir"
	methodWorkspaceRename   = "dalgona/workspace/rename"
	methodWorkspaceRemove   = "dalgona/workspace/remove"
	methodWorkspaceWatch    = "dalgona/workspace/watch"
	methodWorkspaceUnwatch  = "dalgona/workspace/unwatch"
	methodWorkspaceChanged  = "dalgona/workspace/changed"
	methodTerminalOpen      = "dalgona/terminal/open"
	methodTerminalList      = "dalgona/terminal/list"
	methodTerminalAttach    = "dalgona/terminal/attach"
	methodCheckpointOpen    = "dalgona/terminal/checkpoint/open"
	methodCheckpointRead    = "dalgona/terminal/checkpoint/read"
	methodTerminalRead      = "dalgona/terminal/read"
	methodTerminalWrite     = "dalgona/terminal/write"
	methodTerminalResize    = "dalgona/terminal/resize"
	methodTerminalClose     = "dalgona/terminal/close"
	methodTerminalDetach    = "dalgona/terminal/detach"
	methodDiagnosticsRun    = "dalgona/diagnostics/run"
	methodFailuresList      = "dalgona/failures/list"
	methodFailuresRead      = "dalgona/failures/read"
	methodReportingRead     = "dalgona/reporting/read"
	methodReportingEnable   = "dalgona/reporting/enable"
	methodReportingDisable  = "dalgona/reporting/disable"
	featureRunnerManagement = "runnerManagement"
	featureMCPConfig        = "mcpConfig"
	featureAuth             = "auth"
	featureWorkspace        = "workspace"
	featureTerminal         = "terminal"
	featureDiagnostics      = "diagnostics"
	featureFailures         = "failureRecords"
	featureReporting        = "reporting"
	featureReceipts         = "mutationReceipts"
)

type (
	operationRead struct {
		OperationID string `json:"operationId"`
	}
	operationOnly struct {
		OperationID string `json:"operationId"`
	}
	threadOnly struct {
		ThreadID string `json:"threadId"`
	}
)

type (
	remoteStatusParams struct {
		ThreadID string `json:"threadId,omitzero"`
		Cursor   string `json:"cursor,omitzero"`
		Limit    int    `json:"limit,omitzero"`
	}
	remoteRunner struct {
		Name         string   `json:"name"`
		Service      string   `json:"service"`
		Available    bool     `json:"available"`
		Capabilities []string `json:"capabilities"`
	}
	remoteBinding struct {
		ThreadID        string   `json:"threadId"`
		RunnerID        string   `json:"runnerId"`
		BoxID           string   `json:"boxId,omitzero"`
		SharedThreadIDs []string `json:"sharedThreadIds"`
		State           string   `json:"state"`
		BindingRevision string   `json:"bindingRevision"`
	}
	remoteStatus struct {
		Runners        []remoteRunner  `json:"runners"`
		Bindings       []remoteBinding `json:"bindings"`
		LedgerRevision string          `json:"ledgerRevision"`
		NextCursor     string          `json:"nextCursor,omitzero"`
	}
	remotePull struct {
		ExitCode        int      `json:"exitCode"`
		Output          string   `json:"output"`
		ChangedPaths    []string `json:"changedPaths"`
		ConflictPaths   []string `json:"conflictPaths"`
		BindingRevision string   `json:"bindingRevision,omitzero"`
		Truncated       bool     `json:"truncated"`
	}
	gcParams struct {
		Yes             bool   `json:"yes"`
		PreviewRevision string `json:"previewRevision,omitzero"`
	}
	gcCandidate struct {
		BoxID     string   `json:"boxId"`
		Service   string   `json:"service"`
		ThreadIDs []string `json:"threadIds"`
		Reason    string   `json:"reason"`
	}
	gcFailure struct {
		BoxID string `json:"boxId"`
		Kind  string `json:"kind"`
	}
	gcResult struct {
		ExitCode        int           `json:"exitCode"`
		Output          string        `json:"output"`
		Candidates      []gcCandidate `json:"candidates"`
		Destroyed       []string      `json:"destroyed"`
		Failures        []gcFailure   `json:"failures"`
		PreviewRevision string        `json:"previewRevision"`
		Truncated       bool          `json:"truncated"`
	}
	remoteProgress struct {
		OperationID string `json:"operationId"`
		Phase       string `json:"phase"`
		Completed   string `json:"completed"`
		Total       string `json:"total,omitzero"`
	}
)

type (
	mcpReadParams struct {
		ThreadID string `json:"threadId,omitzero"`
	}
	mcpEnvValue struct {
		Value    *string `json:"value,omitzero"`
		Preserve bool    `json:"preserve,omitzero"`
		Redacted bool    `json:"redacted,omitzero"`
	}
	mcpConfig struct {
		Kind              string                 `json:"kind"`
		Command           string                 `json:"command,omitzero"`
		Args              []string               `json:"args,omitzero"`
		Env               map[string]mcpEnvValue `json:"env,omitzero"`
		URL               string                 `json:"url,omitzero"`
		BearerTokenEnv    string                 `json:"bearerTokenEnv,omitzero"`
		OAuth             bool                   `json:"oauth,omitzero"`
		OAuthScopes       []string               `json:"oauthScopes,omitzero"`
		OAuthClientID     string                 `json:"oauthClientId,omitzero"`
		OAuthCallbackPort int                    `json:"oauthCallbackPort,omitzero"`
	}
	mcpServer struct {
		Name      string    `json:"name"`
		Source    string    `json:"source"`
		Editable  bool      `json:"editable"`
		Config    mcpConfig `json:"config"`
		Effective bool      `json:"effective"`
	}
	mcpInventory struct {
		Version          string      `json:"version"`
		Servers          []mcpServer `json:"servers"`
		TrustedWorkspace bool        `json:"trustedWorkspace"`
	}
	mcpEdit struct {
		Name   string     `json:"name"`
		Action string     `json:"action"`
		Config *mcpConfig `json:"config,omitzero"`
	}
	mcpWriteParams struct {
		ExpectedVersion string    `json:"expectedVersion"`
		Edits           []mcpEdit `json:"edits"`
		OperationID     string    `json:"operationId"`
	}
	mcpWritten struct {
		Version                string   `json:"version"`
		RestartRequiredServers []string `json:"restartRequiredServers"`
	}
	mcpStatusParams struct {
		ThreadID      string `json:"threadId"`
		Cursor        string `json:"cursor,omitzero"`
		ConfigVersion string `json:"configVersion"`
	}
	mcpTool struct {
		Name        string `json:"name"`
		Description string `json:"description"`
	}
	mcpRuntime struct {
		Name          string             `json:"name"`
		Source        string             `json:"source"`
		AuthStatus    string             `json:"authStatus"`
		Tools         map[string]mcpTool `json:"tools"`
		RuntimeStatus string             `json:"runtimeStatus"`
		ToolsError    string             `json:"toolsError,omitzero"`
	}
	mcpStatusPage struct {
		Data          []mcpRuntime `json:"data"`
		NextCursor    string       `json:"nextCursor,omitzero"`
		ConfigVersion string       `json:"configVersion"`
	}
)

type (
	authTarget struct {
		Kind     string `json:"kind"`
		ID       string `json:"id,omitzero"`
		Name     string `json:"name,omitzero"`
		ThreadID string `json:"threadId,omitzero"`
	}
	authDescriptor struct {
		Target            authTarget `json:"target"`
		DisplayName       string     `json:"displayName"`
		SignedIn          bool       `json:"signedIn"`
		AccountLabel      string     `json:"accountLabel,omitzero"`
		Flows             []string   `json:"flows"`
		UnavailableReason string     `json:"unavailableReason,omitzero"`
	}
	authProviders struct {
		Targets []authDescriptor `json:"targets"`
	}
	authChallenge struct {
		Kind             string `json:"kind"`
		AuthorizationURL string `json:"authorizationUrl,omitzero"`
		VerificationURL  string `json:"verificationUrl,omitzero"`
		UserCode         string `json:"userCode,omitzero"`
		Prompt           string `json:"prompt,omitzero"`
		ExpiresAt        string `json:"expiresAt"`
	}
	authStatus struct {
		TransactionID string         `json:"transactionId"`
		Target        authTarget     `json:"target"`
		State         string         `json:"state"`
		Challenge     *authChallenge `json:"challenge,omitzero"`
		Error         jsontext.Value `json:"error,omitzero"`
		ExpiresAt     string         `json:"expiresAt"`
	}
	authStartParams struct {
		Target       authTarget `json:"target"`
		Flow         string     `json:"flow"`
		CallbackMode string     `json:"callbackMode,omitzero"`
		OperationID  string     `json:"operationId"`
	}
	authResponse struct {
		Kind  string `json:"kind"`
		Value string `json:"value"`
	}
	authSubmitParams struct {
		TransactionID string       `json:"transactionId"`
		Response      authResponse `json:"response"`
		OperationID   string       `json:"operationId"`
	}
	authTransaction struct {
		TransactionID string `json:"transactionId"`
	}
	authLogoutParams struct {
		Target      authTarget `json:"target"`
		OperationID string     `json:"operationId"`
	}
)

type (
	workspaceOpenParams struct {
		ThreadID    string   `json:"threadId"`
		Root        string   `json:"root"`
		Rights      []string `json:"rights"`
		OperationID string   `json:"operationId"`
	}
	workspaceGrant struct {
		WorkspaceID     string   `json:"workspaceId"`
		ThreadID        string   `json:"threadId"`
		RunnerID        string   `json:"runnerId"`
		BindingRevision string   `json:"bindingRevision"`
		Root            string   `json:"root"`
		PathStyle       string   `json:"pathStyle"`
		Rights          []string `json:"rights"`
		ExpiresAt       string   `json:"expiresAt"`
	}
	workspaceOnly struct {
		WorkspaceID string `json:"workspaceId"`
	}
	workspaceClosed struct {
		Status string `json:"status"`
	}
	fileMetadata struct {
		Name       string `json:"name"`
		Kind       string `json:"kind"`
		Size       string `json:"size"`
		ModifiedAt string `json:"modifiedAt"`
		Revision   string `json:"revision"`
		Readable   bool   `json:"readable"`
		Writable   bool   `json:"writable"`
	}
	listParams struct {
		WorkspaceID string `json:"workspaceId"`
		Path        string `json:"path"`
		Cursor      string `json:"cursor,omitzero"`
		Limit       int    `json:"limit,omitzero"`
	}
	listing struct {
		Entries           []fileMetadata `json:"entries"`
		NextCursor        string         `json:"nextCursor,omitzero"`
		DirectoryRevision string         `json:"directoryRevision"`
	}
	pathParams struct {
		WorkspaceID string `json:"workspaceId"`
		Path        string `json:"path"`
	}
	readParams struct {
		WorkspaceID      string `json:"workspaceId"`
		Path             string `json:"path"`
		ExpectedRevision string `json:"expectedRevision,omitzero"`
		Offset           string `json:"offset,omitzero"`
		Limit            int    `json:"limit,omitzero"`
	}
	readChunk struct {
		Revision   string `json:"revision"`
		Total      string `json:"total"`
		Offset     string `json:"offset"`
		Next       string `json:"next"`
		DataBase64 string `json:"dataBase64"`
	}
	writeBegin struct {
		Stage            string `json:"stage"`
		WorkspaceID      string `json:"workspaceId"`
		Path             string `json:"path"`
		ExpectedRevision string `json:"expectedRevision"`
		Total            string `json:"total"`
		Digest           string `json:"digest"`
		OperationID      string `json:"operationId"`
	}
	writeBegun struct {
		UploadID string `json:"uploadId"`
		Offset   string `json:"offset"`
	}
	writeChunk struct {
		Stage      string `json:"stage"`
		UploadID   string `json:"uploadId"`
		Offset     string `json:"offset"`
		DataBase64 string `json:"dataBase64"`
	}
	writeCommit struct {
		Stage       string `json:"stage"`
		UploadID    string `json:"uploadId"`
		OperationID string `json:"operationId,omitzero"`
	}
	metadataResult struct {
		Metadata fileMetadata `json:"metadata"`
	}
	mkdirParams struct {
		WorkspaceID string `json:"workspaceId"`
		Path        string `json:"path"`
		Recursive   bool   `json:"recursive"`
		OperationID string `json:"operationId"`
	}
	renameParams struct {
		WorkspaceID      string `json:"workspaceId"`
		From             string `json:"from"`
		To               string `json:"to"`
		ExpectedRevision string `json:"expectedRevision"`
		Replace          bool   `json:"replace"`
		OperationID      string `json:"operationId"`
	}
	removeParams struct {
		WorkspaceID      string `json:"workspaceId"`
		Path             string `json:"path"`
		ExpectedRevision string `json:"expectedRevision"`
		Recursive        bool   `json:"recursive"`
		OperationID      string `json:"operationId"`
	}
	watchParams struct {
		WorkspaceID string `json:"workspaceId"`
		Path        string `json:"path"`
		Recursive   bool   `json:"recursive"`
	}
	watching struct {
		WatchID           string `json:"watchId"`
		DirectoryRevision string `json:"directoryRevision"`
	}
	watchOnly struct {
		WatchID string `json:"watchId"`
	}
	workspaceChange struct {
		WatchID           string   `json:"watchId"`
		DirectoryRevision string   `json:"directoryRevision"`
		Paths             []string `json:"paths"`
		RescanRequired    bool     `json:"rescanRequired"`
	}
)

type (
	terminalIdentity struct {
		TerminalID       string `json:"terminalId"`
		DaemonInstanceID string `json:"daemonInstanceId"`
		ThreadID         string `json:"threadId"`
		RunnerID         string `json:"runnerId"`
		BindingRevision  string `json:"bindingRevision"`
		Root             string `json:"root"`
	}
	terminalAttachment struct {
		AttachmentID      string `json:"attachmentId"`
		WorkspaceID       string `json:"workspaceId"`
		WriterLease       string `json:"writerLease,omitzero"`
		AcceptedInputSeq  string `json:"acceptedInputSeq"`
		AcceptedResizeSeq string `json:"acceptedResizeSeq"`
		LeaseExpiresAt    string `json:"leaseExpiresAt"`
	}
	terminalCheckpoint struct {
		CheckpointID  string `json:"checkpointId"`
		Version       int    `json:"version"`
		EngineVersion string `json:"engineVersion"`
		NextOffset    string `json:"nextOffset"`
		ResizeOrdinal string `json:"resizeOrdinal"`
		Columns       int    `json:"columns"`
		Rows          int    `json:"rows"`
		Total         string `json:"total"`
		Digest        string `json:"digest"`
		ExpiresAt     string `json:"expiresAt"`
	}
	terminalStatus struct {
		State              string `json:"state"`
		InputState         string `json:"inputState"`
		ExitCode           *int   `json:"exitCode,omitzero"`
		Signal             string `json:"signal,omitzero"`
		FinalOffset        string `json:"finalOffset,omitzero"`
		FinalResizeOrdinal string `json:"finalResizeOrdinal,omitzero"`
		OutputTruncated    bool   `json:"outputTruncated,omitzero"`
		OutputExpiresAt    string `json:"outputExpiresAt,omitzero"`
	}
	terminalOpened struct {
		Identity   terminalIdentity   `json:"identity"`
		Attachment terminalAttachment `json:"attachment"`
		Checkpoint terminalCheckpoint `json:"checkpoint"`
		Status     terminalStatus     `json:"status"`
	}
	terminalOpenParams struct {
		WorkspaceID string `json:"workspaceId"`
		Columns     int    `json:"columns"`
		Rows        int    `json:"rows"`
		OperationID string `json:"operationId"`
	}
	terminalListing struct {
		Identity       terminalIdentity `json:"identity"`
		Status         terminalStatus   `json:"status"`
		WriterPresent  bool             `json:"writerPresent"`
		LeaseExpiresAt string           `json:"leaseExpiresAt"`
	}
	terminalList struct {
		Terminals []terminalListing `json:"terminals"`
	}
	attachParams struct {
		WorkspaceID string `json:"workspaceId"`
		TerminalID  string `json:"terminalId"`
		Access      string `json:"access"`
		ResumeLease string `json:"resumeLease,omitzero"`
	}
	attachmentRef struct {
		TerminalID   string `json:"terminalId"`
		AttachmentID string `json:"attachmentId"`
	}
	checkpointOpened struct {
		Checkpoint terminalCheckpoint `json:"checkpoint"`
		Status     terminalStatus     `json:"status"`
	}
	checkpointReadParams struct {
		TerminalID   string `json:"terminalId"`
		AttachmentID string `json:"attachmentId"`
		CheckpointID string `json:"checkpointId"`
		Offset       string `json:"offset,omitzero"`
		Limit        int    `json:"limit,omitzero"`
	}
	checkpointChunk struct {
		Offset     string `json:"offset"`
		Next       string `json:"next"`
		Total      string `json:"total"`
		Digest     string `json:"digest"`
		DataBase64 string `json:"dataBase64"`
	}
	terminalReadParams struct {
		TerminalID         string `json:"terminalId"`
		AttachmentID       string `json:"attachmentId"`
		Offset             string `json:"offset"`
		AfterResizeOrdinal string `json:"afterResizeOrdinal"`
		Limit              int    `json:"limit,omitzero"`
		WaitMs             int    `json:"waitMs,omitzero"`
	}
	terminalResize struct {
		Ordinal string `json:"ordinal"`
		Offset  string `json:"offset"`
		Columns int    `json:"columns"`
		Rows    int    `json:"rows"`
	}
	terminalPage struct {
		Offset         string           `json:"offset"`
		Next           string           `json:"next"`
		DataBase64     string           `json:"dataBase64"`
		Resizes        []terminalResize `json:"resizes"`
		ResizeOrdinal  string           `json:"resizeOrdinal"`
		Status         terminalStatus   `json:"status"`
		LeaseExpiresAt string           `json:"leaseExpiresAt"`
	}
	terminalWriteParams struct {
		TerminalID  string `json:"terminalId"`
		WriterLease string `json:"writerLease"`
		InputSeq    string `json:"inputSeq"`
		DataBase64  string `json:"dataBase64"`
	}
	terminalAccepted struct {
		AcceptedSeq string `json:"acceptedSeq"`
	}
	terminalResizeParams struct {
		TerminalID  string `json:"terminalId"`
		WriterLease string `json:"writerLease"`
		ResizeSeq   string `json:"resizeSeq"`
		Columns     int    `json:"columns"`
		Rows        int    `json:"rows"`
	}
	terminalCloseParams struct {
		TerminalID  string `json:"terminalId"`
		OperationID string `json:"operationId"`
	}
	terminalClosed struct {
		Status terminalStatus `json:"status"`
	}
	terminalDetached struct {
		Status string `json:"status"`
	}
)

type (
	diagnosticsParams struct {
		Online bool `json:"online"`
	}
	diagnosticWire struct {
		Status string `json:"status"`
		Check  string `json:"check"`
		Detail string `json:"detail"`
	}
	diagnosticsResult struct {
		Rows     []diagnosticWire `json:"rows"`
		ExitCode int              `json:"exitCode"`
	}
)

type (
	failuresParams struct {
		Cursor string `json:"cursor,omitzero"`
		Limit  int    `json:"limit,omitzero"`
	}
	failureSummary struct {
		ID          string `json:"id"`
		Time        string `json:"time"`
		Product     string `json:"product"`
		Version     string `json:"version"`
		OS          string `json:"os"`
		Arch        string `json:"arch"`
		Class       string `json:"class"`
		Tool        string `json:"tool"`
		Fingerprint string `json:"fingerprint"`
		ContentFree bool   `json:"contentFree"`
		Reportable  bool   `json:"reportable"`
	}
	failuresPage struct {
		Records    []failureSummary `json:"records"`
		NextCursor string           `json:"nextCursor,omitzero"`
	}
	failureID struct {
		ID string `json:"id"`
	}
	failureFrame struct {
		Function string `json:"function"`
		File     string `json:"file"`
		Line     int    `json:"line"`
	}
	failureRecord struct {
		ID          string         `json:"id"`
		Time        string         `json:"time"`
		Class       string         `json:"class"`
		Tool        string         `json:"tool"`
		Error       string         `json:"error"`
		Stack       []failureFrame `json:"stack"`
		Args        string         `json:"args"`
		Output      string         `json:"output"`
		ContentFree bool           `json:"contentFree"`
		Reportable  bool           `json:"reportable"`
	}
	reportingReadParams struct {
		Method string `json:"method,omitzero"`
	}
	consentReceipt struct {
		SchemaVersion int    `json:"schemaVersion"`
		Time          string `json:"time"`
		Method        string `json:"method"`
		TextHash      string `json:"textHash"`
	}
	restriction struct {
		Kind    string `json:"kind"`
		Until   string `json:"until,omitzero"`
		Message string `json:"message"`
	}
	reportingState struct {
		Method         string          `json:"method"`
		Enabled        bool            `json:"enabled"`
		Available      bool            `json:"available"`
		ConsentText    string          `json:"consentText"`
		TextHash       string          `json:"textHash"`
		ConsentReceipt *consentReceipt `json:"consentReceipt"`
		Authorization  string          `json:"authorization"`
		Restriction    *restriction    `json:"restriction"`
		QueueCount     int             `json:"queueCount"`
		UncertainCount int             `json:"uncertainCount"`
	}
	enableParams struct {
		Method       string `json:"method"`
		TextHash     string `json:"textHash"`
		Confirmation string `json:"confirmation"`
		OperationID  string `json:"operationId"`
	}
	enabled struct {
		ConsentReceipt consentReceipt `json:"consentReceipt"`
		Authorization  string         `json:"authorization"`
	}
	disabled struct {
		Enabled                bool `json:"enabled"`
		QueuedReportsDeleted   bool `json:"queuedReportsDeleted"`
		PublishedIssuesDeleted bool `json:"publishedIssuesDeleted"`
	}
)
