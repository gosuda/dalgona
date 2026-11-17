package failure

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"path/filepath"
	"strings"
)

const (
	StatusReady       = "ready"
	StatusUnavailable = "unavailable"
)

const (
	ReasonMissingCorrelation = "failure case unavailable: local correlation is missing"
	ReasonMissingCompletion  = "failure case unavailable: recorded tool completion is missing"
	ReasonFreeform           = "failure case unavailable: freeform tool calls are not supported"
	ReasonNonObject          = "failure case unavailable: tool arguments are not an object"
	ReasonProvider           = "failure case unavailable: provider failures cannot be replayed"
	ReasonPanicNoCall        = "failure case unavailable: panic has no completed tool call"
	ReasonMaskedNotExact     = "failure case unavailable: masked arguments cannot be replayed exactly"
	ReasonMaskingFailed      = "failure case unavailable: masking failed"
	ReasonPreimage           = "failure case unavailable: masked pre-image cannot seed the recorded state"
	ReasonTiming             = "failure case unavailable: timing and concurrency are not recorded"
	ReasonSandbox            = "failure case unavailable: sandbox and process state are not recorded"
	ReasonNetwork            = "failure case unavailable: network and remote service are not recorded"
	ReasonEnv                = "failure case unavailable: environment and external state are not recorded"
	ReasonProviderState      = "failure case unavailable: provider-specific call state is not supported"
	ReasonInlineContent      = "failure case unavailable: inline file content is not included in cases"
	ReasonOverBound          = "failure case unavailable: replay data exceeds its limit"
	ReasonPrivateContent     = "failure case unavailable: public arguments contain private content"
)

const syntheticCallID = "failure-call"

type Correlation struct {
	Session string `json:"session"`
	Turn    string `json:"turn"`
	CallID  string `json:"callID"`
}

type FileRequirement struct {
	Path   string `json:"path"`
	Exists bool   `json:"exists"`
	Bytes  int64  `json:"bytes"`
	SHA256 string `json:"sha256"`
}

type RecordedCall struct {
	ID        string         `json:"id"`
	Name      string         `json:"name"`
	Arguments jsontext.Value `json:"arguments"`
}

type ScriptToolCall struct {
	ID        string         `json:"id"`
	Name      string         `json:"name"`
	Arguments jsontext.Value `json:"arguments"`
}

type ScriptStep struct {
	ToolCall *ScriptToolCall `json:"tool_call,omitempty"`
	Finish   *string         `json:"finish,omitempty"`
	Text     *string         `json:"text,omitempty"`
}

type FailureStep struct {
	SchemaVersion int               `json:"schemaVersion"`
	Status        string            `json:"status"`
	Reason        string            `json:"reason"`
	Call          *RecordedCall     `json:"call,omitempty"`
	Steps         []ScriptStep      `json:"steps,omitempty"`
	Files         []FileRequirement `json:"files,omitempty"`
}

type LocalRecord struct {
	SchemaVersion      int         `json:"schemaVersion"`
	FailureID          string      `json:"failureID"`
	Correlation        Correlation `json:"correlation"`
	CompletionSeq      uint64      `json:"completionSeq"`
	RepairedArgsSHA256 string      `json:"repairedArgsSHA256"`
	OutputBytes        int64       `json:"outputBytes"`
	OutputSHA256       string      `json:"outputSHA256"`
	OS                 string      `json:"os"`
	Arch               string      `json:"arch"`
}

type ConvertInput struct {
	Tool        string
	Freeform    bool
	OrigArgs    jsontext.Value
	Workspace   string
	Completed   bool
	Panic       bool
	Files       []FileRequirement
	Unavailable string
}

func UnavailableStep(reason string) FailureStep {
	return FailureStep{SchemaVersion: schemaVersion, Status: StatusUnavailable, Reason: reason}
}

func BuildFailureStep(ctx context.Context, masker Masker, in ConvertInput) (FailureStep, error) {
	if err := ctx.Err(); err != nil {
		return FailureStep{}, err
	}
	if step, done := unavailableInputStep(in); done {
		return step, nil
	}
	decoded, unavailable, failed := decodeMaskedInput(masker, in)
	if failed {
		return unavailable, nil
	}
	projected, refusal := projectArgs(in.Tool, decoded, in.Workspace)
	if refusal != "" {
		return UnavailableStep(refusal), nil
	}
	encodedArgs, refusal := encodeProjectedArgs(projected)
	if refusal != "" {
		return UnavailableStep(refusal), nil
	}
	files, refusal := projectFiles(in.Files)
	if refusal != "" {
		return UnavailableStep(refusal), nil
	}
	finish := "tool_calls"
	step := FailureStep{
		SchemaVersion: schemaVersion,
		Status:        StatusReady,
		Call:          &RecordedCall{ID: syntheticCallID, Name: in.Tool, Arguments: encodedArgs},
		Steps: []ScriptStep{
			{ToolCall: &ScriptToolCall{ID: syntheticCallID, Name: in.Tool, Arguments: encodedArgs}},
			{Finish: &finish},
		},
		Files: files,
	}
	encoded, err := json.Marshal(step)
	if err != nil || len(encoded) > maxStepJSON {
		return UnavailableStep(ReasonOverBound), nil
	}
	return step, nil
}

func unavailableInputStep(in ConvertInput) (FailureStep, bool) {
	if in.Unavailable != "" {
		return UnavailableStep(in.Unavailable), true
	}
	if in.Panic && !in.Completed {
		return UnavailableStep(ReasonPanicNoCall), true
	}
	if !in.Completed {
		return UnavailableStep(ReasonMissingCompletion), true
	}
	if in.Freeform {
		return UnavailableStep(ReasonFreeform), true
	}
	return FailureStep{}, false
}

func decodeMaskedInput(masker Masker, in ConvertInput) (any, FailureStep, bool) {
	if masker == nil {
		return nil, UnavailableStep(ReasonMaskingFailed), true
	}
	masked, err := masker.MaskWithError(string(in.OrigArgs))
	if err != nil {
		return nil, UnavailableStep(ReasonMaskingFailed), true
	}
	if strings.Contains(masked, "{{") || strings.Contains(masked, "[redacted]") {
		return nil, UnavailableStep(ReasonMaskedNotExact), true
	}
	var decoded any
	if decodeErr := json.Unmarshal([]byte(masked), &decoded); decodeErr != nil {
		return nil, UnavailableStep(ReasonNonObject), true
	}
	return decoded, FailureStep{}, false
}

func encodeProjectedArgs(projected any) (jsontext.Value, string) {
	encodedArgs, err := json.Marshal(projected)
	if err != nil {
		return nil, ReasonOverBound
	}
	if len(encodedArgs) > maxStepArgs {
		return nil, ReasonOverBound
	}
	if hasPrivateShape(string(encodedArgs)) {
		return nil, ReasonPreimage
	}
	return jsontext.Value(encodedArgs), ""
}

func hasPrivateShape(text string) bool {
	rest := tokenPathPattern.ReplaceAllString(text, "${1}[path]")
	if strings.Contains(rest, "[path]") {
		return true
	}
	rest = drivePathPattern.ReplaceAllString(rest, "[path]")
	if strings.Contains(rest, "[path]") {
		return true
	}
	rest = uncPathPattern.ReplaceAllString(rest, "[path]")
	if strings.Contains(rest, "[path]") {
		return true
	}
	if urlPattern.FindString(rest) != "" {
		return true
	}
	if envPattern.FindString(rest) != "" {
		return true
	}
	return false
}

func projectFiles(files []FileRequirement) ([]FileRequirement, string) {
	if len(files) > 32 {
		return nil, ReasonOverBound
	}
	out := make([]FileRequirement, 0, len(files))
	for _, file := range files {
		if refusal := checkFileRequirement(file); refusal != "" {
			return nil, refusal
		}
		clean := file
		clean.SHA256 = strings.ToLower(file.SHA256)
		out = append(out, clean)
	}
	return out, ""
}

func checkFileRequirement(file FileRequirement) string {
	if refusal := checkFilePath(file.Path); refusal != "" {
		return refusal
	}
	if file.Exists && !isHex64(file.SHA256) {
		return ReasonOverBound
	}
	return ""
}

func checkFilePath(path string) string {
	if len(path) == 0 || len(path) > 256 {
		return ReasonOverBound
	}
	if filepath.IsAbs(path) || isAbsolutePath(path) {
		return ReasonPrivateContent
	}
	if path != filepath.ToSlash(filepath.Clean(path)) {
		return ReasonPrivateContent
	}
	if path == ".." || strings.HasPrefix(path, "../") {
		return ReasonPrivateContent
	}
	return ""
}

func isHex64(s string) bool {
	if len(s) != 64 {
		return false
	}
	for i := 0; i < len(s); i++ {
		c := s[i]
		digit := c >= '0' && c <= '9'
		lower := c >= 'a' && c <= 'f'
		upper := c >= 'A' && c <= 'F'
		if !digit && !lower && !upper {
			return false
		}
	}
	return true
}

func projectArgs(name string, decoded any, workspace string) (any, string) {
	obj, ok := decoded.(map[string]any)
	if !ok {
		return projectNonObjectArgs(name)
	}
	switch name {
	case "exec":
		return projectExec(obj, workspace)
	case "read":
		return projectScalars(obj, map[string]bool{"path": true, "offset": true, "limit": true}, workspace, true)
	case "search":
		return projectSearch(obj, workspace)
	case "todo":
		return projectTodo(obj)
	default:
		return projectOpaqueArgs(name, obj, workspace)
	}
}

func projectNonObjectArgs(name string) (any, string) {
	if name == "patch" || name == "rewrite" {
		return nil, ReasonInlineContent
	}
	return nil, ReasonNonObject
}

func projectOpaqueArgs(name string, obj map[string]any, workspace string) (any, string) {
	switch name {
	case "webfetch":
		return projectWebfetch(obj)
	case "patch", "rewrite":
		return projectContent(obj)
	default:
		return nil, ReasonPrivateContent
	}
}

func projectScalars(obj map[string]any, allowed map[string]bool, workspace string, paths bool) (any, string) {
	out := make(map[string]any, len(obj))
	for key, value := range obj {
		if !allowed[key] {
			return nil, ReasonPrivateContent
		}
		mapped, refusal := projectScalarField(key, value, workspace, paths)
		if refusal != "" {
			return nil, refusal
		}
		out[key] = mapped
	}
	return out, ""
}

func projectScalarField(key string, value any, workspace string, paths bool) (any, string) {
	switch item := value.(type) {
	case string:
		if paths && (key == "path" || strings.HasSuffix(key, "path")) {
			return mapPublicPath(item, workspace)
		}
		return item, ""
	case float64, bool, nil:
		return item, ""
	default:
		return nil, ReasonPrivateContent
	}
}

func projectTodo(obj map[string]any) (any, string) {
	out := make(map[string]any, len(obj))
	for key, value := range obj {
		if key != "items" {
			return nil, ReasonPrivateContent
		}
		kept, refusal := projectTodoItems(value)
		if refusal != "" {
			return nil, refusal
		}
		out[key] = kept
	}
	return out, ""
}

func projectTodoItems(value any) (any, string) {
	items, ok := value.([]any)
	if !ok {
		return nil, ReasonPrivateContent
	}
	kept := make([]any, 0, len(items))
	for _, item := range items {
		clean, refusal := projectTodoEntry(item)
		if refusal != "" {
			return nil, refusal
		}
		kept = append(kept, clean)
	}
	return kept, ""
}

func projectTodoEntry(item any) (any, string) {
	entry, ok := item.(map[string]any)
	if !ok {
		return nil, ReasonPrivateContent
	}
	clean := make(map[string]any, len(entry))
	for field, fieldValue := range entry {
		switch leaf := fieldValue.(type) {
		case string, float64, bool, nil:
			clean[field] = leaf
		default:
			return nil, ReasonPrivateContent
		}
	}
	return clean, ""
}

func mapPublicPath(path, workspace string) (string, string) {
	if path == "" {
		return "", ReasonPrivateContent
	}
	if len(path) > 256 {
		return "", ReasonOverBound
	}
	if workspace != "" {
		if rel, ok := workspaceRelative(path, filepath.ToSlash(workspace)); ok {
			return rel, ""
		}
	}
	if filepath.IsAbs(path) || isAbsolutePath(path) {
		return "", ReasonPreimage
	}
	if strings.HasPrefix(path, "~") {
		return "", ReasonPreimage
	}
	return path, ""
}

func projectExec(obj map[string]any, workspace string) (any, string) {
	allowed := map[string]bool{"command": true, "cwd": true, "timeout_ms": true, "escalate": true, "justification": true}
	out := make(map[string]any, len(obj))
	for key, value := range obj {
		if !allowed[key] {
			return nil, ReasonPrivateContent
		}
		if refusal := projectExecField(key, value, workspace, out); refusal != "" {
			return nil, refusal
		}
	}
	if _, ok := out["command"]; !ok {
		return nil, ReasonPrivateContent
	}
	return out, ""
}

func projectExecField(key string, value any, workspace string, out map[string]any) string {
	switch key {
	case "command":
		return projectExecCommand(value, out)
	case "cwd":
		return projectExecCwd(value, workspace, out)
	case "timeout_ms":
		return projectExecNumber(key, value, out)
	case "escalate":
		return projectExecFlag(key, value, out)
	case "justification":
		return projectExecText(key, value, out)
	default:
		return ""
	}
}

func projectExecCommand(value any, out map[string]any) string {
	command, ok := value.(string)
	if !ok || !safeCommand(command) {
		return ReasonPrivateContent
	}
	out["command"] = command
	return ""
}

func projectExecCwd(value any, workspace string, out map[string]any) string {
	cwd, ok := value.(string)
	if !ok {
		return ReasonPrivateContent
	}
	if cwd == "" {
		return ""
	}
	mapped, refusal := mapPublicPath(cwd, workspace)
	if refusal != "" {
		return refusal
	}
	out["cwd"] = mapped
	return ""
}

func projectExecNumber(key string, value any, out map[string]any) string {
	number, ok := value.(float64)
	if !ok {
		return ReasonPrivateContent
	}
	out[key] = number
	return ""
}

func projectExecFlag(key string, value any, out map[string]any) string {
	flag, ok := value.(bool)
	if !ok {
		return ReasonPrivateContent
	}
	out[key] = flag
	return ""
}

func projectExecText(key string, value any, out map[string]any) string {
	text, ok := value.(string)
	if !ok {
		return ReasonPrivateContent
	}
	out[key] = text
	return ""
}

func safeCommand(command string) bool {
	if command == "" || len(command) > 2048 {
		return false
	}
	if !safeCommandChars(command) {
		return false
	}
	fields := strings.Fields(command)
	if len(fields) == 0 {
		return false
	}
	return safeShellBase(fields)
}

func safeCommandChars(command string) bool {
	for _, c := range command {
		switch {
		case c >= 'a' && c <= 'z', c >= 'A' && c <= 'Z', c >= '0' && c <= '9':
		case c == ' ', c == '\t', c == '-', c == '_', c == '.', c == '/', c == ':', c == '+', c == '@', c == '%', c == ',', c == '"', c == '\'':
		default:
			return false
		}
	}
	return true
}

func safeShellBase(fields []string) bool {
	base := fields[0]
	if index := strings.LastIndex(base, "/"); index >= 0 {
		base = base[index+1:]
	}
	switch base {
	case "sh", "bash", "dash", "zsh", "fish", "python", "python3", "node", "deno", "ruby", "perl", "php", "lua":
		return !hasShellRunFlag(fields[1:])
	}
	return true
}

func hasShellRunFlag(fields []string) bool {
	for _, field := range fields {
		if field == "-c" || field == "-e" {
			return true
		}
	}
	return false
}

func projectSearch(obj map[string]any, workspace string) (any, string) {
	allowed := map[string]bool{"mode": true, "pattern": true, "path": true, "glob": true, "literal": true, "ignore_case": true, "kind": true, "lang": true, "limit": true}
	out := make(map[string]any, len(obj))
	for key, value := range obj {
		if !allowed[key] {
			return nil, ReasonPrivateContent
		}
		if refusal := projectSearchField(key, value, workspace, out); refusal != "" {
			return nil, refusal
		}
	}
	return out, ""
}

func projectSearchField(key string, value any, workspace string, out map[string]any) string {
	switch key {
	case "mode":
		return projectSearchMode(value, out)
	case "pattern":
		return projectSearchText(key, value, out)
	case "path", "glob":
		return projectSearchPath(key, value, workspace, out)
	case "kind":
		return projectSearchKind(key, value, out)
	case "lang":
		return projectSearchText(key, value, out)
	case "literal", "ignore_case":
		return projectSearchFlag(key, value, out)
	case "limit":
		return projectSearchLimit(key, value, out)
	default:
		return ""
	}
}

var searchModes = map[string]bool{"find": true, "grep": true, "symbols": true, "definition": true, "syntax": true, "references": true, "importers": true}

func projectSearchMode(value any, out map[string]any) string {
	mode, ok := value.(string)
	if !ok || !searchModes[mode] {
		return ReasonPrivateContent
	}
	out["mode"] = mode
	return ""
}

func projectSearchText(key string, value any, out map[string]any) string {
	text, ok := value.(string)
	if !ok {
		return ReasonPrivateContent
	}
	out[key] = text
	return ""
}

func projectSearchPath(key string, value any, workspace string, out map[string]any) string {
	text, ok := value.(string)
	if !ok {
		return ReasonPrivateContent
	}
	if text == "" {
		return ""
	}
	mapped, refusal := mapPublicPath(text, workspace)
	if refusal != "" {
		return refusal
	}
	out[key] = mapped
	return ""
}

func projectSearchKind(key string, value any, out map[string]any) string {
	switch item := value.(type) {
	case string:
		out[key] = item
		return ""
	case []any:
		for _, kind := range item {
			if _, ok := kind.(string); !ok {
				return ReasonPrivateContent
			}
		}
		out[key] = item
		return ""
	default:
		return ReasonPrivateContent
	}
}

func projectSearchFlag(key string, value any, out map[string]any) string {
	flag, ok := value.(bool)
	if !ok {
		return ReasonPrivateContent
	}
	out[key] = flag
	return ""
}

func projectSearchLimit(key string, value any, out map[string]any) string {
	number, ok := value.(float64)
	if !ok {
		return ReasonPrivateContent
	}
	out[key] = number
	return ""
}

func projectWebfetch(obj map[string]any) (any, string) {
	allowed := map[string]bool{"url": true, "max_bytes": true}
	out := make(map[string]any, len(obj))
	for key, value := range obj {
		if !allowed[key] {
			return nil, ReasonPrivateContent
		}
		var refusal string
		if key == "url" {
			refusal = projectWebfetchURL(value, out)
		} else {
			refusal = projectWebfetchLimit(value, out)
		}
		if refusal != "" {
			return nil, refusal
		}
	}
	if _, ok := out["url"]; !ok {
		return nil, ReasonPrivateContent
	}
	return out, ""
}

func projectWebfetchURL(value any, out map[string]any) string {
	raw, ok := value.(string)
	if !ok || raw == "" {
		return ReasonPrivateContent
	}
	if strings.Contains(raw, "@") || strings.Contains(raw, "?") || strings.Contains(raw, "#") {
		return ReasonPreimage
	}
	out["url"] = raw
	return ""
}

func projectWebfetchLimit(value any, out map[string]any) string {
	number, ok := value.(float64)
	if !ok {
		return ReasonPrivateContent
	}
	out["max_bytes"] = number
	return ""
}

var contentKeys = map[string]bool{
	"text": true, "content": true, "newtext": true, "new_text": true, "old_string": true,
	"new_string": true, "create": true, "insert": true, "replacement": true, "replace": true,
	"body": true, "data": true, "before": true, "after": true, "edits": true, "changes": true,
}

func projectContent(obj map[string]any) (any, string) {
	dropped := false
	out := make(map[string]any, len(obj))
	for key, value := range obj {
		if contentKeys[strings.ToLower(key)] {
			dropped = true
			continue
		}
		mapped, refusal := projectContentField(key, value)
		if refusal != "" {
			return nil, refusal
		}
		out[key] = mapped
	}
	if dropped {
		return out, ReasonInlineContent
	}
	return out, ""
}

func projectContentField(key string, value any) (any, string) {
	switch item := value.(type) {
	case string:
		return projectContentString(key, item)
	case float64, bool, nil:
		return item, ""
	default:
		return nil, ReasonInlineContent
	}
}

func projectContentString(key, item string) (any, string) {
	if len(item) > 128 {
		return nil, ReasonInlineContent
	}
	if key == "path" || strings.HasSuffix(key, "path") {
		rel, refusal := mapPublicPath(item, "")
		if refusal != "" {
			return nil, ReasonInlineContent
		}
		return rel, ""
	}
	return item, ""
}

func ValidateFailureStep(step FailureStep) error {
	if step.SchemaVersion != schemaVersion {
		return invalidStep("schema version must be 1")
	}
	if step.Status != StatusReady && step.Status != StatusUnavailable {
		return invalidStep("status must be ready or unavailable")
	}
	if step.Status == StatusUnavailable {
		return validateUnavailableStep(step)
	}
	return validateReadyStep(step)
}

func invalidStep(detail string) error {
	return errors.New("failure: invalid failure step: " + detail)
}

func validateUnavailableStep(step FailureStep) error {
	if !knownReason(step.Reason) {
		return invalidStep("unavailable reason is not exact")
	}
	if step.Call != nil || len(step.Steps) != 0 || len(step.Files) != 0 {
		return invalidStep("unavailable step carries no call")
	}
	return nil
}

func validateReadyStep(step FailureStep) error {
	if err := validateReadyIdentity(step); err != nil {
		return err
	}
	if err := validateReadyScript(step); err != nil {
		return err
	}
	return validateReadyFiles(step)
}

func validateReadyIdentity(step FailureStep) error {
	if step.Reason != "" {
		return invalidStep("ready step carries no reason")
	}
	if step.Call == nil {
		return invalidStep("ready step needs a call")
	}
	if step.Call.ID != syntheticCallID {
		return invalidStep("call id is synthetic")
	}
	if !toolPattern.MatchString(step.Call.Name) {
		return invalidStep("call name is not valid")
	}
	var decoded any
	if err := json.Unmarshal([]byte(step.Call.Arguments), &decoded); err != nil {
		return invalidStep("call arguments are not JSON")
	}
	if _, ok := decoded.(map[string]any); !ok {
		return invalidStep("call arguments are not an object")
	}
	return nil
}

func validateReadyScript(step FailureStep) error {
	if len(step.Steps) != 2 {
		return invalidStep("ready step needs two script steps")
	}
	if err := validateFirstScriptStep(step); err != nil {
		return err
	}
	return validateSecondScriptStep(step.Steps[1])
}

func validateFirstScriptStep(step FailureStep) error {
	first := step.Steps[0]
	if first.ToolCall == nil || first.Finish != nil || first.Text != nil {
		return invalidStep("first step re-emits the call")
	}
	if first.ToolCall.ID != syntheticCallID || first.ToolCall.Name != step.Call.Name {
		return invalidStep("first step targets the call")
	}
	if string(first.ToolCall.Arguments) != string(step.Call.Arguments) {
		return invalidStep("re-emitted arguments match the call")
	}
	return nil
}

func validateSecondScriptStep(second ScriptStep) error {
	if second.ToolCall != nil || second.Text != nil || second.Finish == nil {
		return invalidStep("second step finishes with tool calls")
	}
	if *second.Finish != "tool_calls" {
		return invalidStep("second step finishes with tool calls")
	}
	return nil
}

func validateReadyFiles(step FailureStep) error {
	if len(step.Files) > 32 {
		return invalidStep("too many file requirements")
	}
	for _, file := range step.Files {
		if len(file.Path) == 0 || len(file.Path) > 256 {
			return invalidStep("file path exceeds its limit")
		}
	}
	encoded, err := json.Marshal(step)
	if err != nil || len(encoded) > maxStepJSON {
		return invalidStep("attachment exceeds its limit")
	}
	return nil
}

func knownReason(reason string) bool {
	switch reason {
	case ReasonMissingCorrelation, ReasonMissingCompletion, ReasonFreeform, ReasonNonObject,
		ReasonProvider, ReasonPanicNoCall, ReasonMaskedNotExact, ReasonMaskingFailed, ReasonPreimage,
		ReasonTiming, ReasonSandbox, ReasonNetwork, ReasonEnv, ReasonProviderState,
		ReasonInlineContent, ReasonOverBound, ReasonPrivateContent:
		return true
	}
	return false
}
