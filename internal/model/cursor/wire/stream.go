package wire

import (
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"google.golang.org/protobuf/encoding/protowire"
)

type TextDelta struct {
	TextView []byte
}

func AppendTextDelta(dst []byte, m *TextDelta) []byte {
	return connectwire.AppendRaw(dst, 1, m.TextView)
}

func ConsumeTextDelta(b []byte, m *TextDelta) error {
	return connectwire.ConsumeFields(b, "cursor text delta", func(f connectwire.Field) error {
		if f.Number != 1 {
			return nil
		}
		return connectwire.SetBytes(f, "cursor text delta", &m.TextView)
	})
}

type ThinkingDelta struct {
	TextView []byte
}

func AppendThinkingDelta(dst []byte, m *ThinkingDelta) []byte {
	return connectwire.AppendRaw(dst, 1, m.TextView)
}

func ConsumeThinkingDelta(b []byte, m *ThinkingDelta) error {
	return connectwire.ConsumeFields(b, "cursor thinking delta", func(f connectwire.Field) error {
		if f.Number != 1 {
			return nil
		}
		return connectwire.SetBytes(f, "cursor thinking delta", &m.TextView)
	})
}

type TokenDelta struct {
	Tokens int32
}

func AppendTokenDelta(dst []byte, m *TokenDelta) []byte {
	return connectwire.AppendInt32(dst, 1, m.Tokens)
}

func ConsumeTokenDelta(b []byte, m *TokenDelta) error {
	return connectwire.ConsumeFields(b, "cursor token delta", func(f connectwire.Field) error {
		if f.Number != 1 {
			return nil
		}
		return connectwire.SetInt32(f, "cursor token delta", &m.Tokens)
	})
}

type McpToolCall struct {
	Args            *McpArgs
	Result          *McpResult
	DescriptionView []byte
}

func AppendMcpToolCall(dst []byte, m *McpToolCall) []byte {
	if m.Args != nil {
		dst = connectwire.AppendMessage(dst, 1, m.Args, AppendMcpArgs)
	}
	if m.Result != nil {
		dst = connectwire.AppendMessage(dst, 2, m.Result, AppendMcpResult)
	}
	return connectwire.AppendRaw(dst, 3, m.DescriptionView)
}

func ConsumeMcpToolCall(b []byte, m *McpToolCall) error {
	return connectwire.ConsumeFields(b, "cursor MCP call", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			if err := connectwire.RequireKind(f, protowire.BytesType, "cursor MCP call"); err != nil {
				return err
			}
			m.Args = new(McpArgs)
			return ConsumeMcpArgs(connectwire.BytesValue(f), m.Args)
		case 2:
			if err := connectwire.RequireKind(f, protowire.BytesType, "cursor MCP call"); err != nil {
				return err
			}
			m.Result = new(McpResult)
			return ConsumeMcpResult(connectwire.BytesValue(f), m.Result)
		case 3:
			return connectwire.SetBytes(f, "cursor MCP call", &m.DescriptionView)
		default:
			return nil
		}
	})
}

type ToolCall struct {
	Mcp            *McpToolCall
	VariantNumber  int32
	VariantView    []byte
	ToolCallIDView []byte
}

func AppendToolCall(dst []byte, m *ToolCall) []byte {
	if m.Mcp != nil {
		dst = connectwire.AppendMessage(dst, 15, m.Mcp, AppendMcpToolCall)
	} else if m.VariantNumber != 0 {
		dst = connectwire.AppendVariant(dst, protowire.Number(m.VariantNumber), m.VariantView)
	}
	return connectwire.AppendRaw(dst, 57, m.ToolCallIDView)
}

func ConsumeToolCall(b []byte, m *ToolCall) error {
	return connectwire.ConsumeFields(b, "cursor tool call", func(f connectwire.Field) error {
		switch f.Number {
		case 15:
			if err := connectwire.RequireKind(f, protowire.BytesType, "cursor tool call"); err != nil {
				return err
			}
			m.Mcp = new(McpToolCall)
			return ConsumeMcpToolCall(connectwire.BytesValue(f), m.Mcp)
		case 57:
			return connectwire.SetBytes(f, "cursor tool call", &m.ToolCallIDView)
		default:
			if f.Kind == protowire.BytesType && f.Number >= 1 && f.Number <= 69 {
				m.VariantNumber = f.Number
				return connectwire.SetBytes(f, "cursor tool call", &m.VariantView)
			}
			return nil
		}
	})
}

type ToolCallStartedUpdate struct {
	CallIDView      []byte
	ToolCall        *ToolCall
	ModelCallIDView []byte
}

func AppendToolCallStartedUpdate(dst []byte, m *ToolCallStartedUpdate) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.CallIDView)
	if m.ToolCall != nil {
		dst = connectwire.AppendMessage(dst, 2, m.ToolCall, AppendToolCall)
	}
	return connectwire.AppendRaw(dst, 3, m.ModelCallIDView)
}

func ConsumeToolCallStartedUpdate(b []byte, m *ToolCallStartedUpdate) error {
	return connectwire.ConsumeFields(b, "cursor tool started", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "cursor tool started", &m.CallIDView)
		case 2:
			if err := connectwire.RequireKind(f, protowire.BytesType, "cursor tool started"); err != nil {
				return err
			}
			m.ToolCall = new(ToolCall)
			return ConsumeToolCall(connectwire.BytesValue(f), m.ToolCall)
		case 3:
			return connectwire.SetBytes(f, "cursor tool started", &m.ModelCallIDView)
		default:
			return nil
		}
	})
}

type ToolCallCompletedUpdate = ToolCallStartedUpdate

func AppendToolCallCompletedUpdate(dst []byte, m *ToolCallCompletedUpdate) []byte {
	return AppendToolCallStartedUpdate(dst, m)
}

func ConsumeToolCallCompletedUpdate(b []byte, m *ToolCallCompletedUpdate) error {
	return ConsumeToolCallStartedUpdate(b, m)
}

type PartialToolCallUpdate struct {
	CallIDView      []byte
	ToolCall        *ToolCall
	ArgsTextView    []byte
	ModelCallIDView []byte
}

func AppendPartialToolCallUpdate(dst []byte, m *PartialToolCallUpdate) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.CallIDView)
	if m.ToolCall != nil {
		dst = connectwire.AppendMessage(dst, 2, m.ToolCall, AppendToolCall)
	}
	dst = connectwire.AppendRaw(dst, 3, m.ArgsTextView)
	return connectwire.AppendRaw(dst, 4, m.ModelCallIDView)
}

func ConsumePartialToolCallUpdate(b []byte, m *PartialToolCallUpdate) error {
	return connectwire.ConsumeFields(b, "cursor partial tool call", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "cursor partial tool call", &m.CallIDView)
		case 2:
			if err := connectwire.RequireKind(f, protowire.BytesType, "cursor partial tool call"); err != nil {
				return err
			}
			m.ToolCall = new(ToolCall)
			return ConsumeToolCall(connectwire.BytesValue(f), m.ToolCall)
		case 3:
			return connectwire.SetBytes(f, "cursor partial tool call", &m.ArgsTextView)
		case 4:
			return connectwire.SetBytes(f, "cursor partial tool call", &m.ModelCallIDView)
		default:
			return nil
		}
	})
}

type ToolCallDeltaUpdate struct {
	CallIDView        []byte
	ToolCallDeltaView []byte
	ModelCallIDView   []byte
}

func AppendToolCallDeltaUpdate(dst []byte, m *ToolCallDeltaUpdate) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.CallIDView)
	dst = connectwire.AppendRaw(dst, 2, m.ToolCallDeltaView)
	return connectwire.AppendRaw(dst, 3, m.ModelCallIDView)
}

func ConsumeToolCallDeltaUpdate(b []byte, m *ToolCallDeltaUpdate) error {
	return connectwire.ConsumeFields(b, "cursor tool delta", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "cursor tool delta", &m.CallIDView)
		case 2:
			return connectwire.SetBytes(f, "cursor tool delta", &m.ToolCallDeltaView)
		case 3:
			return connectwire.SetBytes(f, "cursor tool delta", &m.ModelCallIDView)
		default:
			return nil
		}
	})
}

type Usage struct {
	Input      int64
	Output     int64
	CacheRead  int64
	CacheWrite int64
	Reasoning  int64
}

type TurnEndedUpdate = Usage

func AppendUsage(dst []byte, m *Usage) []byte {
	dst = connectwire.AppendInt64(dst, 1, m.Input)
	dst = connectwire.AppendInt64(dst, 2, m.Output)
	dst = connectwire.AppendInt64(dst, 3, m.CacheRead)
	dst = connectwire.AppendInt64(dst, 4, m.CacheWrite)
	return connectwire.AppendInt64(dst, 5, m.Reasoning)
}

func ConsumeUsage(b []byte, m *Usage) error {
	return connectwire.ConsumeFields(b, "cursor turn end", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetInt64(f, "cursor turn end", &m.Input)
		case 2:
			return connectwire.SetInt64(f, "cursor turn end", &m.Output)
		case 3:
			return connectwire.SetInt64(f, "cursor turn end", &m.CacheRead)
		case 4:
			return connectwire.SetInt64(f, "cursor turn end", &m.CacheWrite)
		case 5:
			return connectwire.SetInt64(f, "cursor turn end", &m.Reasoning)
		default:
			return nil
		}
	})
}

func AppendTurnEndedUpdate(dst []byte, m *TurnEndedUpdate) []byte {
	return AppendUsage(dst, m)
}

func ConsumeTurnEndedUpdate(b []byte, m *TurnEndedUpdate) error {
	return ConsumeUsage(b, m)
}

type InteractionUpdate struct {
	Text          *TextDelta
	Thinking      *ThinkingDelta
	Started       *ToolCallStartedUpdate
	Completed     *ToolCallCompletedUpdate
	Partial       *PartialToolCallUpdate
	TokenDelta    *TokenDelta
	ToolDelta     *ToolCallDeltaUpdate
	Ended         *TurnEndedUpdate
	VariantNumber int32
	VariantView   []byte
}

func AppendInteractionUpdate(dst []byte, m *InteractionUpdate) []byte {
	switch {
	case m.Text != nil:
		return connectwire.AppendMessage(dst, 1, m.Text, AppendTextDelta)
	case m.Thinking != nil:
		return connectwire.AppendMessage(dst, 4, m.Thinking, AppendThinkingDelta)
	case m.TokenDelta != nil:
		return connectwire.AppendMessage(dst, 8, m.TokenDelta, AppendTokenDelta)
	case m.Ended != nil:
		return connectwire.AppendMessage(dst, 14, m.Ended, AppendTurnEndedUpdate)
	}
	return appendInteractionTool(dst, m)
}

func appendInteractionTool(dst []byte, m *InteractionUpdate) []byte {
	switch {
	case m.Started != nil:
		return connectwire.AppendMessage(dst, 2, m.Started, AppendToolCallStartedUpdate)
	case m.Completed != nil:
		return connectwire.AppendMessage(dst, 3, m.Completed, AppendToolCallCompletedUpdate)
	case m.Partial != nil:
		return connectwire.AppendMessage(dst, 7, m.Partial, AppendPartialToolCallUpdate)
	case m.ToolDelta != nil:
		return connectwire.AppendMessage(dst, 15, m.ToolDelta, AppendToolCallDeltaUpdate)
	case m.VariantNumber != 0:
		return connectwire.AppendVariant(dst, protowire.Number(m.VariantNumber), m.VariantView)
	}
	return dst
}

func ConsumeInteractionUpdate(b []byte, m *InteractionUpdate) error {
	return connectwire.ConsumeFields(b, "cursor interaction", func(f connectwire.Field) error {
		if !connectwire.IsVariant(f, 1, 17) {
			return nil
		}
		*m = InteractionUpdate{}
		return consumeInteractionVariant(f, m)
	})
}

func consumeInteractionVariant(f connectwire.Field, m *InteractionUpdate) error {
	switch f.Number {
	case 1:
		return connectwire.ConsumeNested(f, "cursor interaction", &m.Text, ConsumeTextDelta)
	case 2:
		return connectwire.ConsumeNested(f, "cursor interaction", &m.Started, ConsumeToolCallStartedUpdate)
	case 3:
		return connectwire.ConsumeNested(f, "cursor interaction", &m.Completed, ConsumeToolCallCompletedUpdate)
	case 4:
		return connectwire.ConsumeNested(f, "cursor interaction", &m.Thinking, ConsumeThinkingDelta)
	case 7:
		return connectwire.ConsumeNested(f, "cursor interaction", &m.Partial, ConsumePartialToolCallUpdate)
	case 8:
		return connectwire.ConsumeNested(f, "cursor interaction", &m.TokenDelta, ConsumeTokenDelta)
	case 14:
		return connectwire.ConsumeNested(f, "cursor interaction", &m.Ended, ConsumeTurnEndedUpdate)
	case 15:
		return connectwire.ConsumeNested(f, "cursor interaction", &m.ToolDelta, ConsumeToolCallDeltaUpdate)
	}
	m.VariantNumber = f.Number
	return connectwire.SetBytes(f, "cursor interaction", &m.VariantView)
}

type AgentServerMessage struct {
	Interaction   *InteractionUpdate
	Exec          *ExecServerMessage
	MessageNumber int32
	MessageView   []byte
}

func AppendAgentServerMessage(dst []byte, m *AgentServerMessage) []byte {
	if m.Interaction != nil {
		return connectwire.AppendMessage(dst, 1, m.Interaction, AppendInteractionUpdate)
	}
	if m.Exec != nil {
		return connectwire.AppendMessage(dst, 2, m.Exec, AppendExecServerMessage)
	}
	if m.MessageNumber != 0 {
		return connectwire.AppendVariant(dst, protowire.Number(m.MessageNumber), m.MessageView)
	}
	return dst
}

func ConsumeAgentServerMessage(b []byte, m *AgentServerMessage) error {
	return connectwire.ConsumeFields(b, "cursor server message", func(f connectwire.Field) error {
		if !connectwire.IsVariant(f, 1, 7) {
			return nil
		}
		*m = AgentServerMessage{}
		switch f.Number {
		case 1:
			return connectwire.ConsumeNested(f, "cursor server message", &m.Interaction, ConsumeInteractionUpdate)
		case 2:
			return connectwire.ConsumeNested(f, "cursor server message", &m.Exec, ConsumeExecServerMessage)
		}
		m.MessageNumber = f.Number
		return connectwire.SetBytes(f, "cursor server message", &m.MessageView)
	})
}
