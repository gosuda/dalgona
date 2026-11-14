package cursor

import (
	"bytes"
	"cmp"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"slices"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/cursor/wire"
)

const (
	defaultSystemPrompt = "You are a helpful assistant."
	providerIdentifier  = "dalgona"
	nativeShellTool     = "exec"
	maxSchemaDepth      = 64
)

var droppedSchemaKeys = map[string]bool{"oneOf": true, "anyOf": true, "allOf": true}

func buildRun(req model.Request, conv *conversation) ([]byte, error) {
	system := cmp.Or(req.System, defaultSystemPrompt)
	systemID, err := conv.storeJSON(func(encoder *model.JSONEncoder) {
		encoder.BeginObject()
		encoder.StringKey("role", "system")
		encoder.StringKey("content", system)
		encoder.EndObject()
	})
	if err != nil {
		return nil, err
	}
	active, history := splitActive(req.Messages)
	roots, err := rootPrompts(conv, systemID, history, req.Tools)
	if err != nil {
		return nil, err
	}
	action, err := conversationAction(active)
	if err != nil {
		return nil, err
	}
	run := wire.AgentRunRequest{
		Conversation:       conversationState(conv, systemID, roots),
		Action:             action,
		ModelDetails:       &wire.ModelDetails{ModelIDView: []byte(req.Model), DisplayModelIDView: []byte(req.Model)},
		RequestedModel:     &wire.RequestedModel{ModelIDView: []byte(req.Model)},
		ConversationIDView: []byte(conv.id),
	}
	return wire.AppendAgentClientMessage(nil, &wire.AgentClientMessage{Run: &run}), nil
}

func splitActive(messages []model.Message) (string, []model.Message) {
	if len(messages) == 0 {
		return "", nil
	}
	last := messages[len(messages)-1]
	if last.Role == "assistant" || last.Text == "" {
		return "", messages
	}
	history := messages[:len(messages)-1]
	if len(last.ToolResults) != 0 {
		history = append(slices.Clip(history), model.Message{Role: last.Role, ToolResults: last.ToolResults})
	}
	return last.Text, history
}

func conversationAction(active string) (*wire.ConversationAction, error) {
	if active == "" {
		return &wire.ConversationAction{ActionNumber: wire.ResumeActionNumber, ActionView: []byte{}}, nil
	}
	id, err := randomUUID()
	if err != nil {
		return nil, err
	}
	message := &wire.UserMessage{TextView: []byte(active), MessageIDView: []byte(id)}
	return &wire.ConversationAction{User: &wire.UserMessageAction{UserMessage: message}}, nil
}

func conversationState(conv *conversation, systemID []byte, roots [][]byte) *wire.ConversationState {
	var state wire.ConversationState
	if len(conv.checkpoint) != 0 && wire.ConsumeConversationState(conv.checkpoint, &state) == nil &&
		len(state.RootPromptMessages) != 0 && bytes.Equal(state.RootPromptMessages[0], systemID) {
		state.RootPromptMessages, state.Turns = roots, nil
		return &state
	}
	return &wire.ConversationState{RootPromptMessages: roots}
}

func (c *conversation) storeJSON(encode func(*model.JSONEncoder)) ([]byte, error) {
	var buffer bytes.Buffer
	encoder := model.NewJSONEncoder(&buffer)
	encode(encoder)
	if err := encoder.Err(); err != nil {
		return nil, fmt.Errorf("cursor: encode prompt: %w", err)
	}
	return c.storeBlob(bytes.TrimRight(buffer.Bytes(), "\n")), nil
}

func rootPrompts(conv *conversation, systemID []byte, history []model.Message, tools []model.Tool) ([][]byte, error) {
	roots := make([][]byte, 1, len(history)+1)
	roots[0] = systemID
	freeform := model.FreeformToolNames(tools)
	names := make(map[string]string)
	for _, message := range history {
		ids, err := messagePrompts(conv, message, freeform, names)
		if err != nil {
			return nil, err
		}
		roots = append(roots, ids...)
	}
	return roots, nil
}

func messagePrompts(conv *conversation, message model.Message, freeform map[string]bool, names map[string]string) ([][]byte, error) {
	if message.Role == "assistant" {
		for _, call := range message.ToolCalls {
			names[call.ID] = call.Name
		}
		if message.Text == "" && len(message.ToolCalls) == 0 {
			return nil, nil
		}
		id, err := conv.storeJSON(func(encoder *model.JSONEncoder) { encodeAssistant(encoder, message, freeform) })
		return [][]byte{id}, err
	}
	ids := make([][]byte, 0, len(message.ToolResults)+1)
	for _, result := range message.ToolResults {
		id, err := conv.storeJSON(func(encoder *model.JSONEncoder) { encodeToolResult(encoder, result, names) })
		if err != nil {
			return nil, err
		}
		ids = append(ids, id)
	}
	if message.Text == "" {
		return ids, nil
	}
	id, err := conv.storeJSON(func(encoder *model.JSONEncoder) {
		encoder.BeginObject()
		encoder.StringKey("role", "user")
		encoder.Key("content")
		encoder.BeginArray()
		encodeTextPart(encoder, message.Text)
		encoder.EndArray()
		encoder.EndObject()
	})
	return append(ids, id), err
}

func encodeTextPart(encoder *model.JSONEncoder, text string) {
	encoder.BeginObject()
	encoder.StringKey("type", "text")
	encoder.StringKey("text", text)
	encoder.EndObject()
}

func encodeAssistant(encoder *model.JSONEncoder, message model.Message, freeform map[string]bool) {
	encoder.BeginObject()
	encoder.StringKey("role", "assistant")
	encoder.Key("content")
	encoder.BeginArray()
	if message.Text != "" {
		encodeTextPart(encoder, message.Text)
	}
	for _, call := range message.ToolCalls {
		encoder.BeginObject()
		encoder.StringKey("type", "tool-call")
		encoder.StringKey("toolCallId", call.ID)
		encoder.StringKey("toolName", call.Name)
		encoder.Key("args")
		if call.Freeform || freeform[call.Name] {
			encoder.BeginObject()
			encoder.StringKey("input", string(call.Args))
			encoder.EndObject()
		} else {
			encoder.Value(callArgs(call.Args))
		}
		encoder.EndObject()
	}
	encoder.EndArray()
	encoder.EndObject()
}

func callArgs(args jsontext.Value) jsontext.Value {
	if len(args) == 0 {
		return jsontext.Value(`{}`)
	}
	return args
}

func encodeToolResult(encoder *model.JSONEncoder, result model.ToolResult, names map[string]string) {
	name := cmp.Or(result.Name, names[result.ID])
	encoder.BeginObject()
	encoder.StringKey("role", "tool")
	encoder.StringKey("id", result.ID)
	encoder.Key("content")
	encoder.BeginArray()
	encoder.BeginObject()
	encoder.StringKey("type", "tool-result")
	encoder.StringKey("toolName", name)
	encoder.StringKey("toolCallId", result.ID)
	encoder.StringKey("result", model.ToolResultText(result.Output))
	if result.IsError {
		encoder.BoolKey("isError", true)
	}
	encoder.EndObject()
	encoder.EndArray()
	encoder.EndObject()
}

func toolDefinitions(tools []model.Tool) ([]wire.McpToolDefinition, error) {
	definitions := make([]wire.McpToolDefinition, 0, len(tools))
	for _, tool := range tools {
		if tool.Name == nativeShellTool {
			continue
		}
		schema, err := toolSchema(tool)
		if err != nil {
			return nil, err
		}
		value, err := wire.AppendValueFromJSON(nil, schema)
		if err != nil {
			return nil, fmt.Errorf("cursor: tool %s schema: %w", tool.Name, err)
		}
		definitions = append(definitions, wire.McpToolDefinition{
			NameView:               []byte(tool.Name),
			DescriptionView:        []byte(tool.Description),
			InputSchemaView:        value,
			ProviderIdentifierView: []byte(providerIdentifier),
			ToolNameView:           []byte(tool.Name),
		})
	}
	return definitions, nil
}

func toolSchema(tool model.Tool) (jsontext.Value, error) {
	var buffer bytes.Buffer
	encoder := jsontext.NewEncoder(&buffer)
	if tool.Freeform {
		writer := model.NewJSONEncoder(&buffer)
		writer.FreeformSchema()
		return bytes.TrimRight(buffer.Bytes(), "\n"), writer.Err()
	}
	schema := tool.JSONSchema()
	if len(schema) == 0 {
		schema = jsontext.Value(`{"type":"object","properties":{}}`)
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(schema))
	if err := copySchema(encoder, decoder, 0); err != nil {
		return nil, fmt.Errorf("cursor: tool %s schema: %w", tool.Name, err)
	}
	return bytes.TrimRight(buffer.Bytes(), "\n"), nil
}

func copySchema(encoder *jsontext.Encoder, decoder *jsontext.Decoder, depth int) error {
	if depth > maxSchemaDepth {
		return errors.New("schema nesting exceeds 64 levels")
	}
	kind := decoder.PeekKind()
	if kind != jsontext.KindBeginObject && kind != jsontext.KindBeginArray {
		value, err := decoder.ReadValue()
		if err != nil {
			return err
		}
		return encoder.WriteValue(value)
	}
	start, err := decoder.ReadToken()
	if err != nil {
		return err
	}
	if err := encoder.WriteToken(start); err != nil {
		return err
	}
	if kind == jsontext.KindBeginArray {
		return copyItems(encoder, decoder, depth)
	}
	return copyMembers(encoder, decoder, depth)
}

func copyItems(encoder *jsontext.Encoder, decoder *jsontext.Decoder, depth int) error {
	for decoder.PeekKind() != jsontext.KindEndArray {
		if err := copySchema(encoder, decoder, depth+1); err != nil {
			return err
		}
	}
	return copyToken(encoder, decoder)
}

func copyMembers(encoder *jsontext.Encoder, decoder *jsontext.Decoder, depth int) error {
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := decoder.ReadToken()
		if err != nil {
			return err
		}
		if droppedSchemaKeys[name.String()] {
			if err := decoder.SkipValue(); err != nil {
				return err
			}
			continue
		}
		if err := encoder.WriteToken(name); err != nil {
			return err
		}
		if err := copySchema(encoder, decoder, depth+1); err != nil {
			return err
		}
	}
	return copyToken(encoder, decoder)
}

func copyToken(encoder *jsontext.Encoder, decoder *jsontext.Decoder) error {
	token, err := decoder.ReadToken()
	if err != nil {
		return err
	}
	return encoder.WriteToken(token)
}
