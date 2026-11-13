package devin

import (
	"bytes"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	"fmt"
	"runtime"
	"strconv"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/devin/wire"
)

const (
	ideName          = "devin-cli"
	ideType          = "chisel"
	ideVersion       = "3000.11.3"
	sessionPrefix    = "devin-session-token$"
	connectUserAgent = "connect-go/1.18.1 (go1.26.3)"
	requestCascade   = 5
	plannerDefault   = 1
	cacheEphemeral   = 1
	sourceUser       = 1
	sourceAssistant  = 2
	sourceTool       = 4
	toolCallIDMax    = 40
)

var discoveryDisplays = []int32{3, 4, 6, 7, 8}

type credentials struct {
	wireKey string
	jwt     string
}

type chatInput struct {
	credentials
	cascade   string
	execution string
	maxTokens int
}

func devinOS() string {
	switch runtime.GOOS {
	case "darwin", "windows":
		return runtime.GOOS
	default:
		return "linux"
	}
}

func sessionToken(key string) string {
	if len(key) >= len(sessionPrefix) && key[:len(sessionPrefix)] == sessionPrefix {
		return key
	}
	return sessionPrefix + key
}

func metadata(wireKey, jwt string) *wire.Metadata {
	return &wire.Metadata{
		IdeNameView:          []byte(ideName),
		IdeVersionView:       []byte(ideVersion),
		IdeTypeView:          []byte(ideType),
		ExtensionNameView:    []byte(ideType),
		ExtensionVersionView: []byte(ideVersion),
		APIKeyView:           []byte(wireKey),
		LocaleView:           []byte("en"),
		OSView:               []byte(devinOS()),
		UserJWTView:          []byte(jwt),
	}
}

func discoveryMetadata(key string) *wire.Metadata {
	return &wire.Metadata{
		IdeNameView:            []byte(ideType),
		IdeVersionView:         []byte("0.0.0-dev"),
		ExtensionNameView:      []byte(ideType),
		ExtensionVersionView:   []byte("0.0.0-dev"),
		APIKeyView:             []byte(sessionToken(key)),
		LocaleView:             []byte("en"),
		OSView:                 []byte(devinOS()),
		SupportedModelDisplays: discoveryDisplays,
	}
}

func encodeChat(req model.Request, input chatInput) ([]byte, error) {
	prompts, err := buildPrompts(req, input.cascade)
	if err != nil {
		return nil, err
	}
	tools, err := toolDefinitions(req.Tools)
	if err != nil {
		return nil, err
	}
	message := wire.GetChatMessageRequest{
		Metadata:           metadata(input.wireKey, input.jwt),
		PromptView:         []byte(req.System),
		Prompts:            prompts,
		RequestType:        requestCascade,
		Configuration:      completionConfiguration(input.maxTokens),
		Tools:              tools,
		DisableToolCalls:   true,
		ToolChoice:         &wire.ChatToolChoice{ChoiceNumber: 1, ChoiceView: []byte("auto")},
		SystemPromptCache:  &wire.PromptCacheOptions{Type: cacheEphemeral},
		ConversationIDView: []byte(input.cascade),
		PlannerMode:        plannerDefault,
		ChatModelUIDView:   []byte(req.Model),
		ExecutionIDView:    []byte(input.execution),
	}
	return wire.AppendGetChatMessageRequest(nil, &message), nil
}

func completionConfiguration(maxTokens int) *wire.CompletionConfiguration {
	return &wire.CompletionConfiguration{
		NumCompletions:      1,
		MaxTokens:           uint64(max(maxTokens, 1)),
		MaxNewlines:         400,
		Temperature:         1,
		FirstTemperature:    1,
		TopK:                40,
		TopP:                float64(float32(0.95)),
		FimEOTProbThreshold: 1,
	}
}

func toolDefinitions(tools []model.Tool) ([]wire.ChatToolDefinition, error) {
	definitions := make([]wire.ChatToolDefinition, len(tools))
	for index, tool := range tools {
		schema := []byte(tool.JSONSchema())
		if tool.Freeform {
			var buffer bytes.Buffer
			encoder := model.NewJSONEncoder(&buffer)
			encoder.FreeformSchema()
			if err := encoder.Err(); err != nil {
				return nil, fmt.Errorf("devin: encode freeform schema: %w", err)
			}
			schema = buffer.Bytes()
		}
		definitions[index] = wire.ChatToolDefinition{
			NameView:        []byte(tool.Name),
			DescriptionView: []byte(tool.Description),
			JSONSchemaView:  schema,
		}
	}
	return definitions, nil
}

type promptBuilder struct {
	cascade string
	ids     callIDs
	prompts []wire.ChatMessagePrompt
}

func buildPrompts(req model.Request, cascade string) ([]wire.ChatMessagePrompt, error) {
	builder := promptBuilder{cascade: cascade, ids: newCallIDs(), prompts: make([]wire.ChatMessagePrompt, 0, len(req.Messages)+2)}
	for _, message := range req.Messages {
		for _, call := range message.ToolCalls {
			builder.ids.assign(call.ID)
		}
	}
	for index, message := range req.Messages {
		if err := builder.add(index, message); err != nil {
			return nil, err
		}
	}
	return builder.prompts, nil
}

func (b *promptBuilder) add(index int, message model.Message) error {
	if message.Role == "assistant" {
		return b.addAssistant(index, message)
	}
	for _, result := range message.ToolResults {
		if result.ID == "" {
			return fmt.Errorf("devin: tool result %q has no call id", result.Name)
		}
		wireID := b.ids.resolve(result.ID)
		b.prompts = append(b.prompts, wire.ChatMessagePrompt{
			MessageIDView:     []byte(messageID(b.cascade, index, "tool\x00"+wireID)),
			Source:            sourceTool,
			PromptView:        []byte(model.ToolResultText(result.Output)),
			ToolCallIDView:    []byte(wireID),
			ToolResultIsError: result.IsError,
		})
	}
	if message.Text != "" || len(message.ToolResults) == 0 {
		b.prompts = append(b.prompts, wire.ChatMessagePrompt{
			MessageIDView: []byte(messageID(b.cascade, index, "user")),
			Source:        sourceUser,
			PromptView:    []byte(message.Text),
			Images:        devinImages(message.Images),
		})
	}
	return nil
}

func (b *promptBuilder) addAssistant(index int, message model.Message) error {
	text := message.Text
	if message.Reasoning != "" {
		text = message.Reasoning + "\n" + text
	}
	calls := make([]wire.ChatToolCall, 0, len(message.ToolCalls))
	for _, call := range message.ToolCalls {
		if call.ID == "" || call.Name == "" {
			continue
		}
		args, err := callArguments(call)
		if err != nil {
			return err
		}
		calls = append(calls, wire.ChatToolCall{IDView: []byte(b.ids.resolve(call.ID)), NameView: []byte(call.Name), ArgumentsView: args})
	}
	if text == "" && len(calls) == 0 {
		return nil
	}
	b.prompts = append(b.prompts, wire.ChatMessagePrompt{
		MessageIDView: []byte(messageID(b.cascade, index, "assistant")),
		Source:        sourceAssistant,
		PromptView:    []byte(text),
		ToolCalls:     calls,
	})
	return nil
}

func callArguments(call model.ToolCall) ([]byte, error) {
	if !call.Freeform {
		return call.Args, nil
	}
	args := append(make([]byte, 0, len(call.Args)+16), `{"input":`...)
	args, err := jsontext.AppendQuote(args, call.Args)
	if err != nil {
		return nil, fmt.Errorf("devin: encode freeform call %s: %w", call.Name, err)
	}
	return append(args, '}'), nil
}

func devinImages(images []model.Image) []wire.ImageData {
	out := make([]wire.ImageData, len(images))
	for index, image := range images {
		out[index] = wire.ImageData{Base64DataView: []byte(base64.StdEncoding.EncodeToString(image.Data)), MimeTypeView: []byte(image.Mime)}
	}
	return out
}

func messageID(cascade string, index int, role string) string {
	digest := sha256.Sum256([]byte(cascade + "\x00" + strconv.Itoa(index) + "\x00" + role))
	hexed := hex.EncodeToString(digest[:16])
	return hexed[0:8] + "-" + hexed[8:12] + "-" + hexed[12:16] + "-" + hexed[16:20] + "-" + hexed[20:32]
}

type callIDs struct {
	wire map[string]string
	used map[string]bool
}

func newCallIDs() callIDs {
	return callIDs{wire: make(map[string]string), used: make(map[string]bool)}
}

func (c callIDs) assign(original string) {
	if _, ok := c.wire[original]; ok || original == "" {
		return
	}
	base := restrictCallID(original)
	candidate := base
	for counter := 2; c.used[candidate]; counter++ {
		suffix := "_dup" + strconv.Itoa(counter)
		candidate = base[:min(len(base), toolCallIDMax-len(suffix))] + suffix
	}
	c.used[candidate] = true
	c.wire[original] = candidate
}

func (c callIDs) resolve(original string) string {
	c.assign(original)
	return c.wire[original]
}

func restrictCallID(raw string) string {
	out := make([]byte, 0, min(len(raw), toolCallIDMax))
	for _, char := range raw {
		if len(out) == toolCallIDMax {
			break
		}
		if isCallIDChar(char) {
			out = utf8.AppendRune(out, char)
		} else {
			out = append(out, '_')
		}
	}
	return string(out)
}

func isCallIDChar(char rune) bool {
	return char >= 'a' && char <= 'z' || char >= 'A' && char <= 'Z' || char >= '0' && char <= '9' || char == '-' || char == '_'
}
