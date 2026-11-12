package llmstub

import (
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"regexp"

	json "encoding/json/v2"
)

const (
	maxModels = 8
	maxTurns  = 256
	maxSteps  = 512
	maxWhen   = 200
	maxChunks = 16
	maxDelay  = 120000
)

var (
	turnIDPattern     = regexp.MustCompile(`^[a-z0-9-]{1,40}$`)
	retryAfterPattern = regexp.MustCompile(`^[0-9]{1,5}$`)
)

type Script struct {
	Models []Model
	Turns  []Turn
}

type Model struct {
	ID              string
	ContextLength   int
	InputModalities []string
}

type Turn struct {
	ID         string
	When       When
	Once       bool
	Status     int
	RetryAfter string
	Body       jsontext.Value
	Steps      []Step
}

type When struct {
	User       string `json:"user,omitzero"`
	ToolResult string `json:"tool_result,omitzero"`
	AnyText    string `json:"any_text,omitzero"`
}

type Step struct {
	Text       string
	Reasoning  string
	ToolCall   *ToolCall
	DelayMS    int
	Finish     string
	Usage      *Usage
	ErrorFrame *ErrorFrame
	Drop       bool
}

type ToolCall struct {
	ID        string
	Name      string
	Arguments jsontext.Value
	Chunks    int
}

type toolCallFile struct {
	ID        string         `json:"id"`
	Name      string         `json:"name"`
	Arguments jsontext.Value `json:"arguments"`
	Chunks    *int           `json:"chunks,omitzero"`
}

type Usage struct {
	PromptTokens     int `json:"prompt_tokens"`
	CompletionTokens int `json:"completion_tokens"`
}

type ErrorFrame struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

type scriptFile struct {
	Models []modelFile `json:"models"`
	Turns  []turnFile  `json:"turns"`
}

type modelFile struct {
	ID            string `json:"id"`
	ContextLength int    `json:"context_length,omitzero"`
	Architecture  *struct {
		InputModalities []string `json:"input_modalities,omitzero"`
	} `json:"architecture,omitzero"`
}

type turnFile struct {
	ID         string         `json:"id"`
	When       When           `json:"when"`
	Once       bool           `json:"once,omitzero"`
	Status     int            `json:"status,omitzero"`
	RetryAfter string         `json:"retry_after,omitzero"`
	Body       jsontext.Value `json:"body,omitzero"`
	Steps      []stepFile     `json:"steps,omitzero"`
}

type stepFile struct {
	Text       *string       `json:"text,omitzero"`
	Reasoning  *string       `json:"reasoning,omitzero"`
	ToolCall   *toolCallFile `json:"tool_call,omitzero"`
	DelayMS    *int          `json:"delay_ms,omitzero"`
	Finish     *string       `json:"finish,omitzero"`
	Usage      *Usage        `json:"usage,omitzero"`
	ErrorFrame *ErrorFrame   `json:"error_frame,omitzero"`
	Drop       *bool         `json:"drop,omitzero"`
}

func Parse(name string, raw []byte) (Script, error) {
	var file scriptFile
	if err := json.Unmarshal(raw, &file, json.RejectUnknownMembers(true)); err != nil {
		return Script{}, fmt.Errorf("llmstub: %s: %w", name, err)
	}
	if err := validateFile(file); err != nil {
		return Script{}, fmt.Errorf("llmstub: %s: %w", name, err)
	}
	return convert(file), nil
}

func validateFile(file scriptFile) error {
	if err := validateModels(file.Models); err != nil {
		return err
	}
	if n := len(file.Turns); n < 1 || n > maxTurns {
		return fmt.Errorf("turns needs 1 to %d items, got %d", maxTurns, n)
	}
	seen := make(map[string]int, len(file.Turns))
	for i, turn := range file.Turns {
		number := i + 1
		if err := validateTurn(number, turn); err != nil {
			return err
		}
		if first, ok := seen[turn.ID]; ok {
			return turnError(number, fmt.Sprintf("id %q repeats turn %d", turn.ID, first))
		}
		seen[turn.ID] = number
	}
	return nil
}

func validateModels(models []modelFile) error {
	if n := len(models); n < 1 || n > maxModels {
		return fmt.Errorf("models needs 1 to %d items, got %d", maxModels, n)
	}
	seen := make(map[string]bool, len(models))
	for i, model := range models {
		if err := validateModel(i+1, model, seen); err != nil {
			return err
		}
	}
	return nil
}

func validateModel(number int, model modelFile, seen map[string]bool) error {
	if model.ID == "" {
		return fmt.Errorf("model %d: id is empty", number)
	}
	if seen[model.ID] {
		return fmt.Errorf("model %d: id %q repeats", number, model.ID)
	}
	seen[model.ID] = true
	if model.ContextLength < 0 {
		return fmt.Errorf("model %d: context_length is negative", number)
	}
	if model.Architecture == nil {
		return nil
	}
	for _, modality := range model.Architecture.InputModalities {
		if modality != "text" && modality != "image" {
			return fmt.Errorf("model %d: input_modalities entry %q must be text or image", number, modality)
		}
	}
	return nil
}

func validateTurn(turn int, t turnFile) error {
	if !turnIDPattern.MatchString(t.ID) {
		return turnError(turn, "id must match ^[a-z0-9-]{1,40}$")
	}
	if err := validateWhen(t.When); err != nil {
		return turnError(turn, err.Error())
	}
	if err := validateReply(t); err != nil {
		return turnError(turn, err.Error())
	}
	if t.Status != 0 {
		return nil
	}
	return validateSteps(turn, t.Steps)
}

func turnError(turn int, reason string) error {
	return fmt.Errorf("turn %d: %s", turn, reason)
}

func stepError(turn, step int, reason string) error {
	return fmt.Errorf("turn %d: step %d: %s", turn, step, reason)
}

func validateWhen(when When) error {
	values := []string{when.User, when.ToolResult, when.AnyText}
	count := 0
	value := ""
	for _, candidate := range values {
		if candidate != "" {
			count++
			value = candidate
		}
	}
	if count != 1 {
		return errors.New("when needs exactly one of user, tool_result, any_text")
	}
	if len(value) > maxWhen {
		return errors.New("when value must be 1 to 200 bytes")
	}
	return nil
}

func validateReply(t turnFile) error {
	if t.Status != 0 && (t.Status < 400 || t.Status > 599) {
		return errors.New("status must be 0 or 400 to 599")
	}
	if t.Status == 0 {
		return validateTextReply(t)
	}
	return validateStatusReply(t)
}

func validateTextReply(t turnFile) error {
	if len(t.Body) > 0 {
		return errors.New("body needs status")
	}
	return validateRetryAfter(t)
}

func validateStatusReply(t turnFile) error {
	if len(t.Steps) > 0 {
		return errors.New("a status turn takes no steps")
	}
	if len(t.Body) == 0 || !t.Body.IsValid() {
		return errors.New("a status turn needs a valid body")
	}
	return validateRetryAfter(t)
}

func validateRetryAfter(t turnFile) error {
	if t.RetryAfter == "" {
		return nil
	}
	if t.Status != 429 && t.Status != 503 {
		return errors.New("retry_after needs status 429 or 503")
	}
	if !retryAfterPattern.MatchString(t.RetryAfter) {
		return errors.New("retry_after must be 1 to 5 digits")
	}
	return nil
}

func validateSteps(turn int, steps []stepFile) error {
	if n := len(steps); n < 1 || n > maxSteps {
		return turnError(turn, fmt.Sprintf("steps needs 1 to %d items, got %d", maxSteps, n))
	}
	kind := ""
	for i, step := range steps {
		next, err := stepKind(step)
		if err != nil {
			return stepError(turn, i+1, err.Error())
		}
		if err = validateKind(next, step); err != nil {
			return stepError(turn, i+1, err.Error())
		}
		if isTerminal(next) && i != len(steps)-1 {
			return stepError(turn, i+1, "finish, error_frame and drop MUST be the last step")
		}
		kind = next
	}
	if !isTerminal(kind) {
		return stepError(turn, len(steps), "the last step MUST be finish, error_frame or drop")
	}
	return nil
}

func isTerminal(kind string) bool {
	return kind == "finish" || kind == "error_frame" || kind == "drop"
}

func presentKinds(s stepFile) []string {
	names := []string{"text", "reasoning", "tool_call", "delay_ms", "finish", "error_frame", "drop"}
	present := []bool{s.Text != nil, s.Reasoning != nil, s.ToolCall != nil, s.DelayMS != nil, s.Finish != nil, s.ErrorFrame != nil, s.Drop != nil}
	var kinds []string
	for i, ok := range present {
		if ok {
			kinds = append(kinds, names[i])
		}
	}
	return kinds
}

func stepKind(s stepFile) (string, error) {
	kinds := presentKinds(s)
	if len(kinds) != 1 {
		return "", errors.New("needs exactly one of text, reasoning, tool_call, delay_ms, finish, error_frame, drop")
	}
	if s.Usage != nil && kinds[0] != "finish" {
		return "", errors.New("usage needs finish")
	}
	return kinds[0], nil
}

func validateKind(kind string, s stepFile) error {
	switch kind {
	case "tool_call":
		return validateToolCall(*s.ToolCall)
	case "delay_ms":
		return validateDelay(*s.DelayMS)
	case "finish":
		return validateFinish(*s.Finish)
	case "error_frame":
		return validateErrorFrame(*s.ErrorFrame)
	case "drop":
		return validateDrop(*s.Drop)
	}
	return nil
}

func validateDelay(delay int) error {
	if delay < 0 || delay > maxDelay {
		return errors.New("delay_ms must be 0 to 120000")
	}
	return nil
}

func validateErrorFrame(frame ErrorFrame) error {
	if frame.Code == "" {
		return errors.New("error_frame needs a code")
	}
	return nil
}

func validateDrop(drop bool) error {
	if !drop {
		return errors.New("drop must be true")
	}
	return nil
}

func validateToolCall(call toolCallFile) error {
	if call.ID == "" {
		return errors.New("tool_call needs an id")
	}
	if call.Name == "" {
		return errors.New("tool_call needs a name")
	}
	if !call.Arguments.IsValid() || call.Arguments.Kind() != '{' {
		return errors.New("tool_call arguments must be a JSON object")
	}
	if call.Chunks != nil && (*call.Chunks < 1 || *call.Chunks > maxChunks) {
		return errors.New("chunks must be 1 to 16")
	}
	return nil
}

func validateFinish(reason string) error {
	switch reason {
	case "stop", "length", "tool_calls":
		return nil
	default:
		return errors.New("finish must be stop, length or tool_calls")
	}
}

func convert(file scriptFile) Script {
	script := Script{Models: make([]Model, 0, len(file.Models)), Turns: make([]Turn, 0, len(file.Turns))}
	for _, model := range file.Models {
		script.Models = append(script.Models, convertModel(model))
	}
	for _, turn := range file.Turns {
		script.Turns = append(script.Turns, convertTurn(turn))
	}
	return script
}

func convertModel(m modelFile) Model {
	model := Model{ID: m.ID, ContextLength: m.ContextLength}
	if m.Architecture != nil {
		model.InputModalities = m.Architecture.InputModalities
	}
	return model
}

func convertTurn(t turnFile) Turn {
	turn := Turn{ID: t.ID, When: t.When, Once: t.Once, Status: t.Status, RetryAfter: t.RetryAfter, Body: t.Body}
	for _, step := range t.Steps {
		turn.Steps = append(turn.Steps, convertStep(step))
	}
	return turn
}

func convertStep(s stepFile) Step {
	step := Step{Usage: s.Usage, ErrorFrame: s.ErrorFrame}
	if s.ToolCall != nil {
		step.ToolCall = convertToolCall(*s.ToolCall)
	}
	if s.Text != nil {
		step.Text = *s.Text
	}
	if s.Reasoning != nil {
		step.Reasoning = *s.Reasoning
	}
	if s.DelayMS != nil {
		step.DelayMS = *s.DelayMS
	}
	if s.Finish != nil {
		step.Finish = *s.Finish
		step.Usage = finishUsage(s.Usage)
	}
	if s.Drop != nil {
		step.Drop = *s.Drop
	}
	return step
}

func finishUsage(usage *Usage) *Usage {
	if usage != nil {
		return usage
	}
	return &Usage{PromptTokens: 1, CompletionTokens: 1}
}

func convertToolCall(call toolCallFile) *ToolCall {
	converted := &ToolCall{ID: call.ID, Name: call.Name, Arguments: call.Arguments, Chunks: 1}
	if call.Chunks != nil {
		converted.Chunks = *call.Chunks
	}
	return converted
}
