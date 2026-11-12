package event

import (
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"html"
	"math"
	"time"
)

const (
	TypeSessionCreated    = "session.created"
	TypeRunnerBound       = "runner.bound"
	TypeTurnStarted       = "turn.started"
	TypeTurnCompleted     = "turn.completed"
	TypeUserMessage       = "user.message"
	TypeAssistantMessage  = "assistant.message"
	TypeToolStarted       = "tool.started"
	TypeToolCompleted     = "tool.completed"
	TypeModelRetry        = "model.retry"
	TypeApprovalRequested = "approval.requested"
	TypeApprovalResolved  = "approval.resolved"
	TypeCompaction        = "compaction"
	TypeContextPruned     = "context.pruned"
	TypeChildSpawned      = "child.spawned"
	TypeBranch            = "branch"
	TypeScriptStarted     = "script.started"
	TypeScriptEffect      = "script.effect"
	TypeScriptCompleted   = "script.completed"
	TypeAdvisorNote       = "advisor.note"
	TypeThreadNamed       = "thread.named"
	TypeThreadArchived    = "thread.archived"
)

const MaxInlineBytes = 64 << 10

const (
	MaxImages     = 10
	MaxImageBytes = 5 << 20
)

const MaxProjectionBytes = MaxInlineBytes + 4<<10

type Event interface {
	Type() string
	Validate() error
}

type BlobRef struct {
	Blob  string
	Bytes int64
}

type Notice struct {
	Word     string
	Text     string
	TextBlob *BlobRef
}

type Mention struct {
	Path     string
	Text     string
	TextBlob *BlobRef
}

type ToolCall struct {
	ID               string
	Name             string
	Args             jsontext.Value
	ArgsBlob         *BlobRef
	Freeform         bool
	Resolved         bool
	Signature        string
	RedactedThinking []string
}

type Usage struct {
	Input      int
	Output     int
	CacheRead  int
	CacheWrite int
}

type FileChange struct {
	Path     string
	PreSHA   string
	PostSHA  string
	PreBlob  *BlobRef
	PostBlob *BlobRef
}

type Spill struct {
	Path   string
	Bytes  int64
	SHA256 string
}

type LineRange struct {
	Start int
	End   int
}

type Snapshot struct {
	Ref      string
	Path     string
	Blob     string
	Shown    []LineRange
	Revision uint64
	Epoch    uint64
}

type TodoItem struct {
	Text   string
	Status string
}

type SessionCreated struct {
	ID               string
	Parent           string
	ForkOf           string
	Cwd              string
	Model            string
	Approval         string
	Sandbox          string
	Effort           string
	Runner           string
	InstructionsBlob *BlobRef
	Advisor          string
	CreatedAt        time.Time
}

func (SessionCreated) Type() string { return TypeSessionCreated }
func (e SessionCreated) Validate() error {
	if err := required(map[string]string{
		"id": e.ID, "cwd": e.Cwd, "model": e.Model, "runner": e.Runner,
	}); err != nil {
		return err
	}
	if err := oneOf("approval", e.Approval, "auto", "yolo", "ask"); err != nil {
		return err
	}
	if err := oneOf("sandbox", e.Sandbox, "workspace-write", "off"); err != nil {
		return err
	}
	if e.Effort != "" {
		if err := oneOf("effort", e.Effort, "low", "medium", "high", "xhigh", "max"); err != nil {
			return err
		}
	}
	if e.Advisor != "" && e.Parent == "" {
		return errors.New("advisor session needs a parent")
	}
	return validateBlob("instructions_blob", e.InstructionsBlob)
}

type RunnerBound struct {
	Runner   string
	Service  string
	BoxID    string
	Baseline string
}

func (RunnerBound) Type() string { return TypeRunnerBound }
func (e RunnerBound) Validate() error {
	return required(map[string]string{
		"runner": e.Runner, "service": e.Service, "box_id": e.BoxID, "baseline": e.Baseline,
	})
}

type TurnStarted struct {
	TurnID    string
	StartedAt time.Time
}

func (TurnStarted) Type() string { return TypeTurnStarted }
func (e TurnStarted) Validate() error {
	return required(map[string]string{"turn_id": e.TurnID})
}

type TurnCompleted struct {
	TurnID      string
	Status      string
	Error       string
	CompletedAt time.Time
}

func (TurnCompleted) Type() string { return TypeTurnCompleted }
func (e TurnCompleted) Validate() error {
	if err := required(map[string]string{"turn_id": e.TurnID}); err != nil {
		return err
	}
	return oneOf("status", e.Status, "completed", "interrupted", "failed")
}

type UserMessage struct {
	ItemID   string
	TurnID   string
	Text     string
	TextBlob *BlobRef
	Kind     string
	Notices  []Notice
	Mentions []Mention
	Effort   string
	Model    string
	Approval string
	Sandbox  string
	Verbatim bool
}

func (UserMessage) Type() string { return TypeUserMessage }
func (e UserMessage) Validate() error {
	if err := required(map[string]string{"item_id": e.ItemID, "turn_id": e.TurnID}); err != nil {
		return err
	}
	if err := oneOf("kind", e.Kind, "prompt", "steer", "followup"); err != nil {
		return err
	}
	if err := optionalTextPayload("text", e.Text, e.TextBlob); err != nil {
		return err
	}
	if err := validateNotices(e.Notices); err != nil {
		return err
	}
	if err := validateMentions(e.Mentions); err != nil {
		return err
	}
	if e.Verbatim && len(e.Notices)+len(e.Mentions) > 0 {
		return errors.New("a verbatim message must not have notices or mentions")
	}
	return e.validateSettings()
}

func (e UserMessage) validateSettings() error {
	checks := [...]struct {
		name, value string
		allowed     []string
	}{
		{"effort", e.Effort, []string{"low", "medium", "high", "xhigh", "max"}},
		{"approval", e.Approval, []string{"auto", "yolo", "ask"}},
		{"sandbox", e.Sandbox, []string{"workspace-write", "off"}},
	}
	for _, check := range checks {
		if check.value == "" {
			continue
		}
		if err := oneOf(check.name, check.value, check.allowed...); err != nil {
			return err
		}
	}
	return nil
}

func validateNotices(notices []Notice) error {
	for i, notice := range notices {
		if notice.Word == "" {
			return fmt.Errorf("notices[%d]: word must not be empty", i)
		}
		if err := requiredTextPayload("notice text", notice.Text, notice.TextBlob); err != nil {
			return fmt.Errorf("notices[%d]: %w", i, err)
		}
	}
	return nil
}

func validateMentions(mentions []Mention) error {
	for i, mention := range mentions {
		if mention.Path == "" {
			return fmt.Errorf("mentions[%d]: path must not be empty", i)
		}
		if err := optionalTextPayload("mention text", mention.Text, mention.TextBlob); err != nil {
			return fmt.Errorf("mentions[%d]: %w", i, err)
		}
	}
	return nil
}

type AssistantMessage struct {
	ItemID        string
	TurnID        string
	Text          string
	TextBlob      *BlobRef
	Reasoning     string
	ReasoningBlob *BlobRef
	ToolCalls     []ToolCall
	Usage         Usage
	StopReason    string
	ContextSeq    uint64
	ContextWindow int
}

func (AssistantMessage) Type() string { return TypeAssistantMessage }
func (e AssistantMessage) Validate() error {
	if err := required(map[string]string{"item_id": e.ItemID, "turn_id": e.TurnID, "stop_reason": e.StopReason}); err != nil {
		return err
	}
	if err := optionalTextPayload("text", e.Text, e.TextBlob); err != nil {
		return err
	}
	if err := optionalTextPayload("reasoning", e.Reasoning, e.ReasoningBlob); err != nil {
		return err
	}
	for i, call := range e.ToolCalls {
		if err := validateToolCall(i, call); err != nil {
			return err
		}
	}
	if e.ContextWindow < 0 {
		return errors.New("context_window must not be negative")
	}
	return e.Usage.Validate()
}

func validateToolCall(i int, call ToolCall) error {
	if call.ID == "" || call.Name == "" {
		return fmt.Errorf("tool_calls[%d]: id and name must not be empty", i)
	}
	if err := requiredRawPayload("tool_calls args", call.Args, call.ArgsBlob); err != nil {
		return fmt.Errorf("tool_calls[%d]: %w", i, err)
	}
	for j, data := range call.RedactedThinking {
		if data == "" {
			return fmt.Errorf("tool_calls[%d]: redacted_thinking[%d] must not be empty", i, j)
		}
	}
	return nil
}

func (u Usage) Validate() error {
	if u.Input < 0 || u.Output < 0 || u.CacheRead < 0 || u.CacheWrite < 0 {
		return errors.New("usage counts must not be negative")
	}
	return nil
}

type ToolStarted struct {
	CallID    string
	TurnID    string
	Tool      string
	Args      jsontext.Value
	ArgsBlob  *BlobRef
	Class     string
	Repairs   int
	Files     []FileChange
	StartedAt time.Time
}

func (ToolStarted) Type() string { return TypeToolStarted }
func (e ToolStarted) Validate() error {
	if err := required(map[string]string{
		"call_id": e.CallID, "turn_id": e.TurnID, "tool": e.Tool,
	}); err != nil {
		return err
	}
	if err := oneOf("class", e.Class, "read", "edit", "exec", "agent", "script", "remote"); err != nil {
		return err
	}
	if e.Repairs < 0 {
		return errors.New("repairs must not be negative")
	}
	if err := requiredRawPayload("args", e.Args, e.ArgsBlob); err != nil {
		return err
	}
	return validateFileChanges(e.Files)
}

func validateFileChanges(files []FileChange) error {
	for i, file := range files {
		if file.Path == "" {
			return fmt.Errorf("files[%d]: path must not be empty", i)
		}
		if err := optionalSHA("pre_sha", file.PreSHA); err != nil {
			return fmt.Errorf("files[%d]: %w", i, err)
		}
		if err := optionalSHA("post_sha", file.PostSHA); err != nil {
			return fmt.Errorf("files[%d]: %w", i, err)
		}
		if err := validateBlob("pre_blob", file.PreBlob); err != nil {
			return fmt.Errorf("files[%d]: %w", i, err)
		}
		if err := validateBlob("post_blob", file.PostBlob); err != nil {
			return fmt.Errorf("files[%d]: %w", i, err)
		}
	}
	return nil
}

type ToolCompleted struct {
	CallID      string
	OK          bool
	Output      string
	Spill       *Spill
	ExitCode    *int
	Recovered   string
	Meta        jsontext.Value
	CompletedAt time.Time
}

func (ToolCompleted) Type() string { return TypeToolCompleted }
func (e ToolCompleted) Validate() error {
	if e.CallID == "" {
		return errors.New("call_id must not be empty")
	}
	if err := validateOutput(e.Output, e.Spill); err != nil {
		return err
	}
	if err := oneOf("recovered", e.Recovered, "none", "replayed", "interrupted", "skipped"); err != nil {
		return err
	}
	return optionalRawJSON("meta", e.Meta)
}

func validateOutput(output string, spill *Spill) error {
	if spill == nil {
		if len(output) > MaxInlineBytes {
			return errors.New("output exceeds 64 KiB; use spill")
		}
		return nil
	}
	if len(output) > MaxProjectionBytes {
		return errors.New("output exceeds the 64 KiB projection and its notice")
	}
	if spill.Path == "" || spill.Bytes < 0 {
		return errors.New("spill needs a path and a non-negative byte count")
	}
	return validSHA("spill sha256", spill.SHA256)
}

type ModelRetry struct {
	TurnID  string
	Attempt int
	Verdict string
	Status  int
	DelayMS int64
}

func (ModelRetry) Type() string { return TypeModelRetry }
func (e ModelRetry) Validate() error {
	if e.TurnID == "" || e.Attempt < 1 || e.DelayMS < 0 {
		return errors.New("model.retry needs turn_id, a positive attempt, and a non-negative delay_ms")
	}
	return oneOf("verdict", e.Verdict, "retry", "overflow")
}

type ApprovalRequested struct {
	RequestID   string
	CallID      string
	Summary     string
	Class       string
	GrantPrefix string
}

func (ApprovalRequested) Type() string { return TypeApprovalRequested }
func (e ApprovalRequested) Validate() error {
	if err := required(map[string]string{
		"request_id": e.RequestID, "call_id": e.CallID, "summary": e.Summary,
	}); err != nil {
		return err
	}
	if e.Class == "" {
		return nil
	}
	return oneOf("class", e.Class, "read", "edit", "exec", "agent", "script", "remote")
}

type ApprovalResolved struct {
	RequestID string
	Decision  string
	By        string
	Reason    string
	Scores    map[string]float64
}

func (ApprovalResolved) Type() string { return TypeApprovalResolved }
func (e ApprovalResolved) Validate() error {
	if err := required(map[string]string{"request_id": e.RequestID, "by": e.By}); err != nil {
		return err
	}
	if err := oneOf("decision", e.Decision, "accept", "accept_session", "decline", "cancel"); err != nil {
		return err
	}
	for name, score := range e.Scores {
		if name == "" || math.IsNaN(score) || math.IsInf(score, 0) || score < 0 || score > 1 {
			return errors.New("scores must have named probabilities between zero and one")
		}
	}
	return nil
}

type Compaction struct {
	FirstKeptSeq uint64
	Summary      string
	Method       string
	TokensBefore int
	TokensAfter  int
}

func (Compaction) Type() string { return TypeCompaction }
func (e Compaction) Validate() error {
	if e.FirstKeptSeq == 0 || e.Summary == "" || e.TokensBefore < 0 || e.TokensAfter < 0 {
		return errors.New("compaction needs first_kept_seq, summary, and non-negative token counts")
	}
	return oneOf("method", e.Method, "auto", "manual", "overflow")
}

type ContextPruned struct {
	TurnID  string
	CallIDs []string
}

func (ContextPruned) Type() string { return TypeContextPruned }
func (e ContextPruned) Validate() error {
	if e.TurnID == "" || len(e.CallIDs) == 0 {
		return errors.New("context.pruned needs turn_id and at least one call id")
	}
	for i, id := range e.CallIDs {
		if id == "" {
			return fmt.Errorf("call_ids[%d] must not be empty", i)
		}
	}
	return nil
}

type ChildSpawned struct {
	CallID  string
	ChildID string
	Runner  string
}

func (ChildSpawned) Type() string { return TypeChildSpawned }
func (e ChildSpawned) Validate() error {
	return required(map[string]string{"call_id": e.CallID, "child_id": e.ChildID, "runner": e.Runner})
}

type Branch struct{ ParentSeq uint64 }

func (Branch) Type() string { return TypeBranch }
func (Branch) Validate() error {
	return nil
}

type ScriptStarted struct {
	RunID      string
	SourceBlob BlobRef
	Entry      string
	Args       jsontext.Value
	ArgsBlob   *BlobRef
	Seed       uint64
}

func (ScriptStarted) Type() string { return TypeScriptStarted }
func (e ScriptStarted) Validate() error {
	if err := required(map[string]string{"run_id": e.RunID, "entry": e.Entry}); err != nil {
		return err
	}
	if err := validateBlob("source_blob", &e.SourceBlob); err != nil {
		return err
	}
	return requiredRawPayload("args", e.Args, e.ArgsBlob)
}

type ScriptEffect struct {
	RunID      string
	N          uint64
	Op         string
	ArgsSHA    string
	Result     jsontext.Value
	ResultBlob *BlobRef
	Error      string
	Denied     bool
}

func (ScriptEffect) Type() string { return TypeScriptEffect }
func (e ScriptEffect) Validate() error {
	if err := required(map[string]string{"run_id": e.RunID, "op": e.Op}); err != nil {
		return err
	}
	if err := validSHA("args_sha", e.ArgsSHA); err != nil {
		return err
	}
	if e.Denied && e.Error == "" {
		return errors.New("a denied script effect must have an error")
	}
	if e.Error == "" {
		return requiredRawPayload("result", e.Result, e.ResultBlob)
	}
	if len(e.Result) != 0 || e.ResultBlob != nil {
		return errors.New("a failed script effect must not have a result")
	}
	return nil
}

type ScriptCompleted struct {
	RunID     string
	OK        bool
	Value     jsontext.Value
	ValueBlob *BlobRef
	Error     string
	Status    string
	Code      string
	Stage     string
}

func (ScriptCompleted) Type() string { return TypeScriptCompleted }
func (e ScriptCompleted) Validate() error {
	if e.RunID == "" {
		return errors.New("run_id must not be empty")
	}
	if e.OK && e.Error != "" {
		return errors.New("a successful script must not have an error")
	}
	if !e.OK && e.Error == "" {
		return errors.New("a failed script needs an error")
	}
	if e.OK {
		return requiredRawPayload("value", e.Value, e.ValueBlob)
	}
	if len(e.Value) != 0 || e.ValueBlob != nil {
		return errors.New("a failed script must not have a value")
	}
	return nil
}

type AdvisorNote struct {
	Advisor   string
	Severity  string
	Note      string
	ReviewSeq uint64
	Delivery  string
}

func (AdvisorNote) Type() string { return TypeAdvisorNote }
func (e AdvisorNote) Validate() error {
	if err := required(map[string]string{"advisor": e.Advisor, "note": e.Note}); err != nil {
		return err
	}
	if len(e.Note) > MaxInlineBytes {
		return errors.New("advisor note exceeds 64 KiB")
	}
	if err := oneOf("severity", e.Severity, "nit", "concern", "blocker"); err != nil {
		return err
	}
	return oneOf("delivery", e.Delivery, "steer", "aside", "continue")
}

func (e AdvisorNote) Render() string {
	return `<advisory advisor="` + html.EscapeString(e.Advisor) + `" severity="` + html.EscapeString(e.Severity) + `">` +
		html.EscapeString(e.Note) + `</advisory>`
}

const MaxThreadNameBytes = 1024

type ThreadNamed struct{ Name string }

func (ThreadNamed) Type() string { return TypeThreadNamed }
func (e ThreadNamed) Validate() error {
	if len(e.Name) > MaxThreadNameBytes {
		return errors.New("thread name exceeds 1 KiB")
	}
	return nil
}

type ThreadArchived struct{ Archived bool }

func (ThreadArchived) Type() string { return TypeThreadArchived }
func (ThreadArchived) Validate() error {
	return nil
}

func required(fields map[string]string) error {
	for name, value := range fields {
		if value == "" {
			return fmt.Errorf("%s must not be empty", name)
		}
	}
	return nil
}

func oneOf(name, value string, allowed ...string) error {
	for _, candidate := range allowed {
		if value == candidate {
			return nil
		}
	}
	return fmt.Errorf("%s has invalid value %q", name, value)
}

func optionalTextPayload(name, text string, blob *BlobRef) error {
	if blob == nil {
		if len(text) > MaxInlineBytes {
			return fmt.Errorf("%s exceeds 64 KiB; use a blob reference", name)
		}
		return nil
	}
	if text != "" {
		return fmt.Errorf("%s must be inline or a blob, not both", name)
	}
	return validateBlob(name, blob)
}

func requiredTextPayload(name, text string, blob *BlobRef) error {
	if text == "" && blob == nil {
		return fmt.Errorf("%s must not be empty", name)
	}
	return optionalTextPayload(name, text, blob)
}

func requiredRawPayload(name string, value jsontext.Value, blob *BlobRef) error {
	if blob == nil {
		return requiredRawJSON(name, value)
	}
	if len(value) != 0 {
		return fmt.Errorf("%s must be inline or a blob, not both", name)
	}
	return validateBlob(name, blob)
}

func requiredRawJSON(name string, value jsontext.Value) error {
	if len(value) == 0 {
		return fmt.Errorf("%s must contain JSON", name)
	}
	return validateRawJSON(name, value)
}

func optionalRawJSON(name string, value jsontext.Value) error {
	if len(value) == 0 {
		return nil
	}
	return validateRawJSON(name, value)
}

func validateRawJSON(name string, value jsontext.Value) error {
	if len(value) > MaxInlineBytes {
		return fmt.Errorf("%s exceeds 64 KiB; use a blob reference", name)
	}
	if !value.IsValid() {
		return fmt.Errorf("%s must contain valid JSON", name)
	}
	return nil
}

func validateBlob(name string, blob *BlobRef) error {
	if blob == nil {
		return nil
	}
	if blob.Bytes < 0 {
		return fmt.Errorf("%s bytes must not be negative", name)
	}
	return validSHA(name, blob.Blob)
}

func optionalSHA(name, value string) error {
	if value == "" {
		return nil
	}
	return validSHA(name, value)
}

func validSHA(name, value string) error {
	if len(value) != 64 {
		return fmt.Errorf("%s must be a lowercase SHA-256 digest", name)
	}
	for _, c := range []byte(value) {
		if (c < '0' || c > '9') && (c < 'a' || c > 'f') {
			return fmt.Errorf("%s must be a lowercase SHA-256 digest", name)
		}
	}
	return nil
}
