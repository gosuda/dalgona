package wire

import (
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"google.golang.org/protobuf/encoding/protowire"
)

type ConversationState struct {
	RootPromptMessages [][]byte
	TurnsOld           [][]byte
	Todos              [][]byte
	PendingToolCalls   [][]byte
	TokenDetailsView   []byte
	SummaryView        []byte
	PlanView           []byte
	Turns              [][]byte
	PreviousWorkspaces [][]byte
	Mode               int32
	SummaryArchiveView []byte
	SummaryArchives    [][]byte
	SelfSummaryCount   uint32
	ReadPaths          [][]byte
}

func AppendConversationState(dst []byte, m *ConversationState) []byte {
	dst = connectwire.AppendRepeatedBytes(dst, 1, m.RootPromptMessages)
	dst = connectwire.AppendRepeatedBytes(dst, 2, m.TurnsOld)
	dst = connectwire.AppendRepeatedBytes(dst, 3, m.Todos)
	dst = connectwire.AppendRepeatedBytes(dst, 4, m.PendingToolCalls)
	dst = connectwire.AppendRaw(dst, 5, m.TokenDetailsView)
	dst = connectwire.AppendRaw(dst, 6, m.SummaryView)
	dst = connectwire.AppendRaw(dst, 7, m.PlanView)
	dst = connectwire.AppendRepeatedBytes(dst, 8, m.Turns)
	dst = connectwire.AppendRepeatedBytes(dst, 9, m.PreviousWorkspaces)
	dst = connectwire.AppendInt32(dst, 10, m.Mode)
	dst = connectwire.AppendRaw(dst, 11, m.SummaryArchiveView)
	dst = connectwire.AppendRepeatedBytes(dst, 13, m.SummaryArchives)
	dst = connectwire.AppendUint32(dst, 17, m.SelfSummaryCount)
	return connectwire.AppendRepeatedBytes(dst, 18, m.ReadPaths)
}

func ConsumeConversationState(b []byte, m *ConversationState) error {
	return connectwire.ConsumeFields(b, "cursor conversation", func(f connectwire.Field) error {
		switch {
		case f.Number <= 5:
			return consumeConversationBase(f, m)
		case f.Number <= 9:
			return consumeConversationTurns(f, m)
		default:
			return consumeConversationExtra(f, m)
		}
	})
}

func consumeConversationBase(f connectwire.Field, m *ConversationState) error {
	switch f.Number {
	case 1:
		return appendConversationBytes(f, &m.RootPromptMessages)
	case 2:
		return appendConversationBytes(f, &m.TurnsOld)
	case 3:
		return appendConversationBytes(f, &m.Todos)
	case 4:
		return appendConversationBytes(f, &m.PendingToolCalls)
	case 5:
		return connectwire.SetBytes(f, "cursor conversation", &m.TokenDetailsView)
	default:
		return nil
	}
}

func consumeConversationTurns(f connectwire.Field, m *ConversationState) error {
	switch f.Number {
	case 6:
		return connectwire.SetBytes(f, "cursor conversation", &m.SummaryView)
	case 7:
		return connectwire.SetBytes(f, "cursor conversation", &m.PlanView)
	case 8:
		return appendConversationBytes(f, &m.Turns)
	case 9:
		return appendConversationBytes(f, &m.PreviousWorkspaces)
	default:
		return nil
	}
}

func consumeConversationExtra(f connectwire.Field, m *ConversationState) error {
	switch f.Number {
	case 10:
		return connectwire.SetInt32(f, "cursor conversation", &m.Mode)
	case 11:
		return connectwire.SetBytes(f, "cursor conversation", &m.SummaryArchiveView)
	case 13:
		return appendConversationBytes(f, &m.SummaryArchives)
	case 17:
		return connectwire.SetUint32(f, "cursor conversation", &m.SelfSummaryCount)
	case 18:
		return appendConversationBytes(f, &m.ReadPaths)
	default:
		return nil
	}
}

func appendConversationBytes(f connectwire.Field, out *[][]byte) error {
	if err := connectwire.RequireKind(f, protowire.BytesType, "cursor conversation"); err != nil {
		return err
	}
	*out = append(*out, connectwire.BytesValue(f))
	return nil
}

type UserMessage struct {
	TextView            []byte
	MessageIDView       []byte
	SelectedContextView []byte
	Mode                int32
	Simulated           bool
	BestOfNGroupIDView  []byte
	TryBestOfN          bool
	RichTextView        []byte
}

func AppendUserMessage(dst []byte, m *UserMessage) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.TextView)
	dst = connectwire.AppendRaw(dst, 2, m.MessageIDView)
	dst = connectwire.AppendRaw(dst, 3, m.SelectedContextView)
	dst = connectwire.AppendInt32(dst, 4, m.Mode)
	dst = connectwire.AppendBool(dst, 5, m.Simulated)
	dst = connectwire.AppendRaw(dst, 6, m.BestOfNGroupIDView)
	dst = connectwire.AppendBool(dst, 7, m.TryBestOfN)
	return connectwire.AppendRaw(dst, 8, m.RichTextView)
}

func ConsumeUserMessage(b []byte, m *UserMessage) error {
	return connectwire.ConsumeFields(b, "cursor user message", func(f connectwire.Field) error {
		if f.Number <= 4 {
			return consumeUserMessageBase(f, m)
		}
		return consumeUserMessageOptions(f, m)
	})
}

func consumeUserMessageBase(f connectwire.Field, m *UserMessage) error {
	switch f.Number {
	case 1:
		return connectwire.SetBytes(f, "cursor user message", &m.TextView)
	case 2:
		return connectwire.SetBytes(f, "cursor user message", &m.MessageIDView)
	case 3:
		return connectwire.SetBytes(f, "cursor user message", &m.SelectedContextView)
	case 4:
		return connectwire.SetInt32(f, "cursor user message", &m.Mode)
	default:
		return nil
	}
}

func consumeUserMessageOptions(f connectwire.Field, m *UserMessage) error {
	switch f.Number {
	case 5:
		return connectwire.SetBool(f, "cursor user message", &m.Simulated)
	case 6:
		return connectwire.SetBytes(f, "cursor user message", &m.BestOfNGroupIDView)
	case 7:
		return connectwire.SetBool(f, "cursor user message", &m.TryBestOfN)
	case 8:
		return connectwire.SetBytes(f, "cursor user message", &m.RichTextView)
	default:
		return nil
	}
}

type UserMessageAction struct {
	UserMessage               *UserMessage
	RequestContextView        []byte
	SendToInteractionListener bool
}

func AppendUserMessageAction(dst []byte, m *UserMessageAction) []byte {
	if m.UserMessage != nil {
		dst = connectwire.AppendMessage(dst, 1, m.UserMessage, AppendUserMessage)
	}
	dst = connectwire.AppendRaw(dst, 2, m.RequestContextView)
	return connectwire.AppendBool(dst, 3, m.SendToInteractionListener)
}

func ConsumeUserMessageAction(b []byte, m *UserMessageAction) error {
	return connectwire.ConsumeFields(b, "cursor user action", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			if err := connectwire.RequireKind(f, protowire.BytesType, "cursor user action"); err != nil {
				return err
			}
			m.UserMessage = new(UserMessage)
			return ConsumeUserMessage(connectwire.BytesValue(f), m.UserMessage)
		case 2:
			return connectwire.SetBytes(f, "cursor user action", &m.RequestContextView)
		case 3:
			return connectwire.SetBool(f, "cursor user action", &m.SendToInteractionListener)
		default:
			return nil
		}
	})
}

type ConversationAction struct {
	ActionNumber int32
	User         *UserMessageAction
	ActionView   []byte
}

func AppendConversationAction(dst []byte, m *ConversationAction) []byte {
	if m.User != nil {
		return connectwire.AppendMessage(dst, 1, m.User, AppendUserMessageAction)
	}
	if m.ActionNumber != 0 {
		return connectwire.AppendVariant(dst, protowire.Number(m.ActionNumber), m.ActionView)
	}
	return dst
}

func ConsumeConversationAction(b []byte, m *ConversationAction) error {
	return connectwire.ConsumeFields(b, "cursor action", func(f connectwire.Field) error {
		if !connectwire.IsVariant(f, 1, 8) {
			return nil
		}
		*m = ConversationAction{}
		if f.Number == 1 {
			return connectwire.ConsumeNested(f, "cursor action", &m.User, ConsumeUserMessageAction)
		}
		m.ActionNumber = f.Number
		return connectwire.SetBytes(f, "cursor action", &m.ActionView)
	})
}

type McpToolDefinition struct {
	NameView               []byte
	DescriptionView        []byte
	InputSchemaView        []byte
	ProviderIdentifierView []byte
	ToolNameView           []byte
	InputSchemaJSONView    []byte
}

func AppendMcpToolDefinition(dst []byte, m *McpToolDefinition) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.NameView)
	dst = connectwire.AppendRaw(dst, 2, m.DescriptionView)
	dst = connectwire.AppendRaw(dst, 3, m.InputSchemaView)
	dst = connectwire.AppendRaw(dst, 4, m.ProviderIdentifierView)
	dst = connectwire.AppendRaw(dst, 5, m.ToolNameView)
	return connectwire.AppendRaw(dst, 6, m.InputSchemaJSONView)
}

func ConsumeMcpToolDefinition(b []byte, m *McpToolDefinition) error {
	return connectwire.ConsumeFields(b, "cursor MCP definition", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "cursor MCP definition", &m.NameView)
		case 2:
			return connectwire.SetBytes(f, "cursor MCP definition", &m.DescriptionView)
		case 3:
			return connectwire.SetBytes(f, "cursor MCP definition", &m.InputSchemaView)
		case 4:
			return connectwire.SetBytes(f, "cursor MCP definition", &m.ProviderIdentifierView)
		case 5:
			return connectwire.SetBytes(f, "cursor MCP definition", &m.ToolNameView)
		case 6:
			return connectwire.SetBytes(f, "cursor MCP definition", &m.InputSchemaJSONView)
		default:
			return nil
		}
	})
}

type AgentRunRequest struct {
	Conversation           *ConversationState
	Action                 *ConversationAction
	ModelDetails           *ModelDetails
	Tools                  []McpToolDefinition
	ConversationIDView     []byte
	FileSystemOptionsView  []byte
	SkillOptionsView       []byte
	CustomSystemPromptView []byte
	RequestedModel         *RequestedModel
}

func AppendAgentRunRequest(dst []byte, m *AgentRunRequest) []byte {
	if m.Conversation != nil {
		dst = connectwire.AppendMessage(dst, 1, m.Conversation, AppendConversationState)
	}
	if m.Action != nil {
		dst = connectwire.AppendMessage(dst, 2, m.Action, AppendConversationAction)
	}
	if m.ModelDetails != nil {
		dst = connectwire.AppendMessage(dst, 3, m.ModelDetails, AppendModelDetails)
	}
	if m.Tools != nil {
		dst = connectwire.AppendMessage(dst, 4, &m.Tools, appendMcpTools)
	}
	dst = connectwire.AppendRaw(dst, 5, m.ConversationIDView)
	dst = connectwire.AppendRaw(dst, 6, m.FileSystemOptionsView)
	dst = connectwire.AppendRaw(dst, 7, m.SkillOptionsView)
	dst = connectwire.AppendRaw(dst, 8, m.CustomSystemPromptView)
	if m.RequestedModel != nil {
		dst = connectwire.AppendMessage(dst, 9, m.RequestedModel, AppendRequestedModel)
	}
	return dst
}

func appendMcpTools(dst []byte, tools *[]McpToolDefinition) []byte {
	for i := range *tools {
		dst = connectwire.AppendMessage(dst, 1, &(*tools)[i], AppendMcpToolDefinition)
	}
	return dst
}

func ConsumeAgentRunRequest(b []byte, m *AgentRunRequest) error {
	return connectwire.ConsumeFields(b, "cursor run request", func(f connectwire.Field) error {
		if f.Number <= 4 {
			return consumeRunRequestCore(f, m)
		}
		return consumeRunRequestOptions(f, m)
	})
}

func consumeRunRequestCore(f connectwire.Field, m *AgentRunRequest) error {
	switch f.Number {
	case 1:
		return consumeConversationMessage(f, m)
	case 2:
		return consumeActionMessage(f, m)
	case 3:
		return consumeModelDetailsMessage(f, m)
	case 4:
		return consumeMcpTools(f, m)
	default:
		return nil
	}
}

func consumeRunRequestOptions(f connectwire.Field, m *AgentRunRequest) error {
	switch f.Number {
	case 5:
		return connectwire.SetBytes(f, "cursor run request", &m.ConversationIDView)
	case 6:
		return connectwire.SetBytes(f, "cursor run request", &m.FileSystemOptionsView)
	case 7:
		return connectwire.SetBytes(f, "cursor run request", &m.SkillOptionsView)
	case 8:
		return connectwire.SetBytes(f, "cursor run request", &m.CustomSystemPromptView)
	case 9:
		if err := connectwire.RequireKind(f, protowire.BytesType, "cursor run request"); err != nil {
			return err
		}
		m.RequestedModel = new(RequestedModel)
		return ConsumeRequestedModel(connectwire.BytesValue(f), m.RequestedModel)
	default:
		return nil
	}
}

func consumeConversationMessage(f connectwire.Field, m *AgentRunRequest) error {
	if err := connectwire.RequireKind(f, protowire.BytesType, "cursor run request"); err != nil {
		return err
	}
	m.Conversation = new(ConversationState)
	return ConsumeConversationState(connectwire.BytesValue(f), m.Conversation)
}

func consumeActionMessage(f connectwire.Field, m *AgentRunRequest) error {
	if err := connectwire.RequireKind(f, protowire.BytesType, "cursor run request"); err != nil {
		return err
	}
	m.Action = new(ConversationAction)
	return ConsumeConversationAction(connectwire.BytesValue(f), m.Action)
}

func consumeModelDetailsMessage(f connectwire.Field, m *AgentRunRequest) error {
	if err := connectwire.RequireKind(f, protowire.BytesType, "cursor run request"); err != nil {
		return err
	}
	m.ModelDetails = new(ModelDetails)
	return ConsumeModelDetails(connectwire.BytesValue(f), m.ModelDetails)
}

func consumeMcpTools(f connectwire.Field, m *AgentRunRequest) error {
	if err := connectwire.RequireKind(f, protowire.BytesType, "cursor run request"); err != nil {
		return err
	}
	m.Tools = make([]McpToolDefinition, 0)
	return connectwire.ConsumeFields(connectwire.BytesValue(f), "cursor MCP tools", func(inner connectwire.Field) error {
		if inner.Number != 1 {
			return nil
		}
		if err := connectwire.RequireKind(inner, protowire.BytesType, "cursor MCP tools"); err != nil {
			return err
		}
		var definition McpToolDefinition
		if err := ConsumeMcpToolDefinition(connectwire.BytesValue(inner), &definition); err != nil {
			return err
		}
		m.Tools = append(m.Tools, definition)
		return nil
	})
}

type AgentClientMessage struct {
	Run           *AgentRunRequest
	Exec          *ExecClientMessage
	Action        *ConversationAction
	MessageNumber int32
	MessageView   []byte
}

func AppendAgentClientMessage(dst []byte, m *AgentClientMessage) []byte {
	if m.Run != nil {
		return connectwire.AppendMessage(dst, 1, m.Run, AppendAgentRunRequest)
	}
	if m.Exec != nil {
		return connectwire.AppendMessage(dst, 2, m.Exec, AppendExecClientMessage)
	}
	if m.Action != nil {
		return connectwire.AppendMessage(dst, 4, m.Action, AppendConversationAction)
	}
	if m.MessageNumber != 0 {
		return connectwire.AppendVariant(dst, protowire.Number(m.MessageNumber), m.MessageView)
	}
	return dst
}

func ConsumeAgentClientMessage(b []byte, m *AgentClientMessage) error {
	return connectwire.ConsumeFields(b, "cursor client message", func(f connectwire.Field) error {
		if !connectwire.IsVariant(f, 1, 8) {
			return nil
		}
		*m = AgentClientMessage{}
		switch f.Number {
		case 1:
			return connectwire.ConsumeNested(f, "cursor client message", &m.Run, ConsumeAgentRunRequest)
		case 2:
			return connectwire.ConsumeNested(f, "cursor client message", &m.Exec, ConsumeExecClientMessage)
		case 4:
			return connectwire.ConsumeNested(f, "cursor client message", &m.Action, ConsumeConversationAction)
		}
		m.MessageNumber = f.Number
		return connectwire.SetBytes(f, "cursor client message", &m.MessageView)
	})
}
