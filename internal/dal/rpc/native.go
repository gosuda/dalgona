package rpc

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"strconv"
	"time"
)

const (
	NativeVersion       = 1
	NativeDelivery      = "ordered-v1"
	MethodEvent         = "dalgona/event"
	MethodRequest       = "dalgona/request"
	MethodCancelRequest = "$/cancelRequest"
	EventEnvelopeLimit  = 256 << 10
	TextDeltaLimit      = 64 << 10
	RequestHeaderLimit  = 4096
	NativeQueueBytes    = 16 << 20
	IDLimit             = 128
)

type ErrorKind string

const (
	ErrorInvalidParams     ErrorKind = "invalid_params"
	ErrorUnavailable       ErrorKind = "unavailable"
	ErrorIncompatible      ErrorKind = "incompatible"
	ErrorStaleRevision     ErrorKind = "stale_revision"
	ErrorPermissionDenied  ErrorKind = "permission_denied"
	ErrorLimitExceeded     ErrorKind = "limit_exceeded"
	ErrorExpired           ErrorKind = "expired"
	ErrorBusy              ErrorKind = "busy"
	ErrorUnknownOutcome    ErrorKind = "unknown_outcome"
	ErrorReplayGap         ErrorKind = "replay_gap"
	ErrorCancelled         ErrorKind = "cancelled"
	ErrorNotFound          ErrorKind = "not_found"
	ErrorOperationConflict ErrorKind = "operation_conflict"
)

var errorCodes = map[ErrorKind]int{
	ErrorInvalidParams:     CodeInvalidParams,
	ErrorUnavailable:       -32010,
	ErrorIncompatible:      -32011,
	ErrorStaleRevision:     -32012,
	ErrorPermissionDenied:  -32013,
	ErrorLimitExceeded:     -32014,
	ErrorExpired:           -32015,
	ErrorBusy:              -32016,
	ErrorUnknownOutcome:    -32017,
	ErrorReplayGap:         -32018,
	ErrorCancelled:         -32019,
	ErrorNotFound:          -32020,
	ErrorOperationConflict: -32021,
}

type ErrorData struct {
	Kind            ErrorKind `json:"kind"`
	OperationID     string    `json:"operationId,omitzero"`
	Stage           string    `json:"stage,omitzero"`
	CurrentRevision string    `json:"currentRevision,omitzero"`
	RetryAfterMs    int64     `json:"retryAfterMs,omitzero"`
}

func NativeError(data ErrorData) *RPCError {
	code, ok := errorCodes[data.Kind]
	if !ok {
		return &RPCError{Code: CodeInternal, Message: "unregistered error kind " + string(data.Kind)}
	}
	raw, err := json.Marshal(data)
	if err != nil {
		return &RPCError{Code: code, Message: string(data.Kind)}
	}
	return &RPCError{Code: code, Message: string(data.Kind), Data: raw}
}

func NativeKind(kind ErrorKind) *RPCError { return NativeError(ErrorData{Kind: kind}) }

func ErrorDataOf(err error) (ErrorData, bool) {
	coded, ok := errors.AsType[*RPCError](err)
	if !ok || len(coded.Data) == 0 {
		return ErrorData{}, false
	}
	var data ErrorData
	if json.Unmarshal(coded.Data, &data) != nil || errorCodes[data.Kind] != coded.Code {
		return ErrorData{}, false
	}
	return data, true
}

func FormatDecimal(n uint64) string { return strconv.FormatUint(n, 10) }

func ParseDecimal(s string) (uint64, error) {
	n, err := strconv.ParseUint(s, 10, 64)
	if err != nil || strconv.FormatUint(n, 10) != s {
		return 0, fmt.Errorf("%q is not a canonical unsigned decimal", s)
	}
	return n, nil
}

func FormatTimestamp(t time.Time) string { return t.UTC().Format("2006-01-02T15:04:05.000Z07:00") }

type (
	NativeCapability struct {
		Version  int    `json:"version"`
		Delivery string `json:"delivery"`
	}
	NativeSession struct {
		Version          int    `json:"version"`
		Delivery         string `json:"delivery"`
		DaemonInstanceID string `json:"daemonInstanceId"`
		StreamID         string `json:"streamId"`
	}
	Platform struct {
		OS        string `json:"os"`
		Arch      string `json:"arch"`
		PathStyle string `json:"pathStyle"`
	}
	CapabilityLimits struct {
		FrameBytes       int `json:"frameBytes"`
		ResultBytes      int `json:"resultBytes"`
		Subscriptions    int `json:"subscriptions"`
		SnapshotHandles  int `json:"snapshotHandles"`
		WorkspaceHandles int `json:"workspaceHandles"`
		TerminalSessions int `json:"terminalSessions"`
	}
	Capabilities struct {
		Version                   int              `json:"version"`
		DaemonInstanceID          string           `json:"daemonInstanceId"`
		BackendBuild              string           `json:"backendBuild"`
		Platform                  Platform         `json:"platform"`
		Features                  map[string]int   `json:"features"`
		ReceiptMethods            []string         `json:"receiptMethods"`
		TerminalCheckpointVersion int              `json:"terminalCheckpointVersion,omitzero"`
		Limits                    CapabilityLimits `json:"limits"`
	}
	ContentSource struct {
		Kind      string `json:"kind"`
		HistoryID string `json:"historyId,omitzero"`
		StreamID  string `json:"streamId,omitzero"`
	}
	ContentReference struct {
		Source    ContentSource `json:"source"`
		ContentID string        `json:"contentId"`
		Field     string        `json:"field"`
		Encoding  string        `json:"encoding"`
		Total     string        `json:"total"`
		Digest    string        `json:"digest"`
	}
	ItemHeader struct {
		ID           string                    `json:"id"`
		Type         string                    `json:"type"`
		ItemRevision string                    `json:"itemRevision"`
		Metadata     map[string]jsontext.Value `json:"metadata"`
		Contents     []ContentReference        `json:"contents"`
	}
	EventParams struct {
		StreamID      string            `json:"streamId"`
		FirstOrdinal  string            `json:"firstOrdinal"`
		LastOrdinal   string            `json:"lastOrdinal"`
		ThreadID      string            `json:"threadId"`
		Method        string            `json:"method"`
		Params        jsontext.Value    `json:"params,omitzero"`
		ParamsContent *ContentReference `json:"paramsContent,omitzero"`
	}
	RequestDetails struct {
		RequestID string `json:"requestId"`
		Total     string `json:"total"`
		Digest    string `json:"digest"`
	}
	EventPosition struct {
		StreamID string `json:"streamId"`
		Ordinal  string `json:"ordinal"`
		ThreadID string `json:"threadId"`
	}
	RequestParams struct {
		Method       string         `json:"method"`
		Details      RequestDetails `json:"details"`
		DalgonaEvent EventPosition  `json:"dalgonaEvent"`
	}
	ContentSummary struct {
		Total  string `json:"total"`
		Digest string `json:"digest"`
	}
	PendingRequest struct {
		RequestID string         `json:"requestId"`
		Method    string         `json:"method"`
		Details   ContentSummary `json:"details"`
	}
	ContentChunk struct {
		DataBase64 string `json:"dataBase64"`
		Offset     string `json:"offset"`
		Next       string `json:"next"`
		Total      string `json:"total"`
		Digest     string `json:"digest"`
	}
	HistorySnapshot struct {
		HistoryID       string           `json:"historyId"`
		Thread          Thread           `json:"thread"`
		PathRevision    string           `json:"pathRevision"`
		StreamID        string           `json:"streamId"`
		CutOrdinal      string           `json:"cutOrdinal"`
		ActiveTurnID    string           `json:"activeTurnId,omitzero"`
		PendingRequests []PendingRequest `json:"pendingRequests"`
		NewestCursor    string           `json:"newestCursor"`
		OldestCursor    string           `json:"oldestCursor"`
		ExpiresAt       string           `json:"expiresAt"`
	}
	HistoryTurn struct {
		TurnID         string       `json:"turnId"`
		TurnRevision   string       `json:"turnRevision"`
		Status         string       `json:"status"`
		Error          *TurnError   `json:"error,omitzero"`
		StartedAt      int64        `json:"startedAt,omitzero"`
		CompletedAt    int64        `json:"completedAt,omitzero"`
		DurationMs     int64        `json:"durationMs,omitzero"`
		FirstItemIndex int          `json:"firstItemIndex"`
		NextItemIndex  int          `json:"nextItemIndex"`
		ItemsComplete  bool         `json:"itemsComplete"`
		Items          []ItemHeader `json:"items"`
	}
	HistoryPage struct {
		Turns          []HistoryTurn `json:"turns"`
		NextCursor     string        `json:"nextCursor,omitzero"`
		PreviousCursor string        `json:"previousCursor,omitzero"`
		PathRevision   string        `json:"pathRevision"`
	}
	OperationRecord struct {
		State     string         `json:"state"`
		Method    string         `json:"method"`
		Result    jsontext.Value `json:"result,omitzero"`
		Error     *RPCError      `json:"error,omitzero"`
		ExpiresAt string         `json:"expiresAt"`
	}
)

type Event struct {
	First, Last   uint64
	ThreadID      string
	Method        string
	Params        jsontext.Value
	ParamsContent *ContentReference
}

var ErrStreamBroken = errors.New("ordered stream broken")

func ParseEvent(streamID string, raw jsontext.Value) (Event, error) {
	var p EventParams
	if err := json.Unmarshal(raw, &p); err != nil {
		return Event{}, fmt.Errorf("%w: decode event: %w", ErrStreamBroken, err)
	}
	if p.StreamID != streamID {
		return Event{}, fmt.Errorf("%w: stream %q, want %q", ErrStreamBroken, p.StreamID, streamID)
	}
	first, err := ParseDecimal(p.FirstOrdinal)
	if err != nil {
		return Event{}, fmt.Errorf("%w: firstOrdinal: %w", ErrStreamBroken, err)
	}
	last, err := ParseDecimal(p.LastOrdinal)
	if err != nil {
		return Event{}, fmt.Errorf("%w: lastOrdinal: %w", ErrStreamBroken, err)
	}
	if first == 0 || last < first || (len(p.Params) == 0) == (p.ParamsContent == nil) {
		return Event{}, fmt.Errorf("%w: impossible event range or params", ErrStreamBroken)
	}
	return Event{First: first, Last: last, ThreadID: p.ThreadID, Method: p.Method, Params: p.Params, ParamsContent: p.ParamsContent}, nil
}

func (e Event) AfterCut(cut uint64) (bool, error) {
	switch {
	case e.Last <= cut:
		return false, nil
	case e.First > cut:
		return true, nil
	}
	return false, fmt.Errorf("%w: envelope %d-%d straddles cut %d", ErrStreamBroken, e.First, e.Last, cut)
}
