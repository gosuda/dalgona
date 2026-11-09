package wire

import "google.golang.org/protobuf/encoding/protowire"

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
	dst = appendRepeatedBytes(dst, 1, m.RootPromptMessages)
	dst = appendRepeatedBytes(dst, 2, m.TurnsOld)
	dst = appendRepeatedBytes(dst, 3, m.Todos)
	dst = appendRepeatedBytes(dst, 4, m.PendingToolCalls)
	dst = appendRaw(dst, 5, m.TokenDetailsView)
	dst = appendRaw(dst, 6, m.SummaryView)
	dst = appendRaw(dst, 7, m.PlanView)
	dst = appendRepeatedBytes(dst, 8, m.Turns)
	dst = appendRepeatedBytes(dst, 9, m.PreviousWorkspaces)
	dst = appendInt32(dst, 10, m.Mode)
	dst = appendRaw(dst, 11, m.SummaryArchiveView)
	dst = appendRepeatedBytes(dst, 13, m.SummaryArchives)
	dst = appendUint32(dst, 17, m.SelfSummaryCount)
	return appendRepeatedBytes(dst, 18, m.ReadPaths)
}

func ConsumeConversationState(b []byte, m *ConversationState) error {
	return consumeFields(b, "cursor conversation", func(f field) error {
		switch {
		case f.number <= 5:
			return consumeConversationBase(f, m)
		case f.number <= 9:
			return consumeConversationTurns(f, m)
		default:
			return consumeConversationExtra(f, m)
		}
	})
}

func consumeConversationBase(f field, m *ConversationState) error {
	switch f.number {
	case 1:
		return appendConversationBytes(f, &m.RootPromptMessages)
	case 2:
		return appendConversationBytes(f, &m.TurnsOld)
	case 3:
		return appendConversationBytes(f, &m.Todos)
	case 4:
		return appendConversationBytes(f, &m.PendingToolCalls)
	case 5:
		return setBytes(f, "cursor conversation", &m.TokenDetailsView)
	default:
		return nil
	}
}

func consumeConversationTurns(f field, m *ConversationState) error {
	switch f.number {
	case 6:
		return setBytes(f, "cursor conversation", &m.SummaryView)
	case 7:
		return setBytes(f, "cursor conversation", &m.PlanView)
	case 8:
		return appendConversationBytes(f, &m.Turns)
	case 9:
		return appendConversationBytes(f, &m.PreviousWorkspaces)
	default:
		return nil
	}
}

func consumeConversationExtra(f field, m *ConversationState) error {
	switch f.number {
	case 10:
		return setInt32(f, "cursor conversation", &m.Mode)
	case 11:
		return setBytes(f, "cursor conversation", &m.SummaryArchiveView)
	case 13:
		return appendConversationBytes(f, &m.SummaryArchives)
	case 17:
		return setUint32(f, "cursor conversation", &m.SelfSummaryCount)
	case 18:
		return appendConversationBytes(f, &m.ReadPaths)
	default:
		return nil
	}
}

func appendConversationBytes(f field, out *[][]byte) error {
	if err := requireKind(f, protowire.BytesType, "cursor conversation"); err != nil {
		return err
	}
	*out = append(*out, bytesValue(f))
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
	dst = appendRaw(dst, 1, m.TextView)
	dst = appendRaw(dst, 2, m.MessageIDView)
	dst = appendRaw(dst, 3, m.SelectedContextView)
	dst = appendInt32(dst, 4, m.Mode)
	dst = appendBool(dst, 5, m.Simulated)
	dst = appendRaw(dst, 6, m.BestOfNGroupIDView)
	dst = appendBool(dst, 7, m.TryBestOfN)
	return appendRaw(dst, 8, m.RichTextView)
}

func ConsumeUserMessage(b []byte, m *UserMessage) error {
	return consumeFields(b, "cursor user message", func(f field) error {
		if f.number <= 4 {
			return consumeUserMessageBase(f, m)
		}
		return consumeUserMessageOptions(f, m)
	})
}

func consumeUserMessageBase(f field, m *UserMessage) error {
	switch f.number {
	case 1:
		return setBytes(f, "cursor user message", &m.TextView)
	case 2:
		return setBytes(f, "cursor user message", &m.MessageIDView)
	case 3:
		return setBytes(f, "cursor user message", &m.SelectedContextView)
	case 4:
		return setInt32(f, "cursor user message", &m.Mode)
	default:
		return nil
	}
}

func consumeUserMessageOptions(f field, m *UserMessage) error {
	switch f.number {
	case 5:
		return setBool(f, "cursor user message", &m.Simulated)
	case 6:
		return setBytes(f, "cursor user message", &m.BestOfNGroupIDView)
	case 7:
		return setBool(f, "cursor user message", &m.TryBestOfN)
	case 8:
		return setBytes(f, "cursor user message", &m.RichTextView)
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
		dst = appendMessage(dst, 1, m.UserMessage, AppendUserMessage)
	}
	dst = appendRaw(dst, 2, m.RequestContextView)
	return appendBool(dst, 3, m.SendToInteractionListener)
}

func ConsumeUserMessageAction(b []byte, m *UserMessageAction) error {
	return consumeFields(b, "cursor user action", func(f field) error {
		switch f.number {
		case 1:
			if err := requireKind(f, protowire.BytesType, "cursor user action"); err != nil {
				return err
			}
			m.UserMessage = new(UserMessage)
			return ConsumeUserMessage(bytesValue(f), m.UserMessage)
		case 2:
			return setBytes(f, "cursor user action", &m.RequestContextView)
		case 3:
			return setBool(f, "cursor user action", &m.SendToInteractionListener)
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
		return appendMessage(dst, 1, m.User, AppendUserMessageAction)
	}
	if m.ActionNumber != 0 {
		return appendVariant(dst, protowire.Number(m.ActionNumber), m.ActionView)
	}
	return dst
}

func ConsumeConversationAction(b []byte, m *ConversationAction) error {
	return consumeFields(b, "cursor action", func(f field) error {
		if !isVariant(f, 1, 8) {
			return nil
		}
		*m = ConversationAction{}
		if f.number == 1 {
			return consumeNested(f, "cursor action", &m.User, ConsumeUserMessageAction)
		}
		m.ActionNumber = f.number
		return setBytes(f, "cursor action", &m.ActionView)
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
	dst = appendRaw(dst, 1, m.NameView)
	dst = appendRaw(dst, 2, m.DescriptionView)
	dst = appendRaw(dst, 3, m.InputSchemaView)
	dst = appendRaw(dst, 4, m.ProviderIdentifierView)
	dst = appendRaw(dst, 5, m.ToolNameView)
	return appendRaw(dst, 6, m.InputSchemaJSONView)
}

func ConsumeMcpToolDefinition(b []byte, m *McpToolDefinition) error {
	return consumeFields(b, "cursor MCP definition", func(f field) error {
		switch f.number {
		case 1:
			return setBytes(f, "cursor MCP definition", &m.NameView)
		case 2:
			return setBytes(f, "cursor MCP definition", &m.DescriptionView)
		case 3:
			return setBytes(f, "cursor MCP definition", &m.InputSchemaView)
		case 4:
			return setBytes(f, "cursor MCP definition", &m.ProviderIdentifierView)
		case 5:
			return setBytes(f, "cursor MCP definition", &m.ToolNameView)
		case 6:
			return setBytes(f, "cursor MCP definition", &m.InputSchemaJSONView)
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
		dst = appendMessage(dst, 1, m.Conversation, AppendConversationState)
	}
	if m.Action != nil {
		dst = appendMessage(dst, 2, m.Action, AppendConversationAction)
	}
	if m.ModelDetails != nil {
		dst = appendMessage(dst, 3, m.ModelDetails, AppendModelDetails)
	}
	if m.Tools != nil {
		dst = appendMessage(dst, 4, &m.Tools, appendMcpTools)
	}
	dst = appendRaw(dst, 5, m.ConversationIDView)
	dst = appendRaw(dst, 6, m.FileSystemOptionsView)
	dst = appendRaw(dst, 7, m.SkillOptionsView)
	dst = appendRaw(dst, 8, m.CustomSystemPromptView)
	if m.RequestedModel != nil {
		dst = appendMessage(dst, 9, m.RequestedModel, AppendRequestedModel)
	}
	return dst
}

func appendMcpTools(dst []byte, tools *[]McpToolDefinition) []byte {
	for i := range *tools {
		dst = appendMessage(dst, 1, &(*tools)[i], AppendMcpToolDefinition)
	}
	return dst
}

func ConsumeAgentRunRequest(b []byte, m *AgentRunRequest) error {
	return consumeFields(b, "cursor run request", func(f field) error {
		if f.number <= 4 {
			return consumeRunRequestCore(f, m)
		}
		return consumeRunRequestOptions(f, m)
	})
}

func consumeRunRequestCore(f field, m *AgentRunRequest) error {
	switch f.number {
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

func consumeRunRequestOptions(f field, m *AgentRunRequest) error {
	switch f.number {
	case 5:
		return setBytes(f, "cursor run request", &m.ConversationIDView)
	case 6:
		return setBytes(f, "cursor run request", &m.FileSystemOptionsView)
	case 7:
		return setBytes(f, "cursor run request", &m.SkillOptionsView)
	case 8:
		return setBytes(f, "cursor run request", &m.CustomSystemPromptView)
	case 9:
		if err := requireKind(f, protowire.BytesType, "cursor run request"); err != nil {
			return err
		}
		m.RequestedModel = new(RequestedModel)
		return ConsumeRequestedModel(bytesValue(f), m.RequestedModel)
	default:
		return nil
	}
}

func consumeConversationMessage(f field, m *AgentRunRequest) error {
	if err := requireKind(f, protowire.BytesType, "cursor run request"); err != nil {
		return err
	}
	m.Conversation = new(ConversationState)
	return ConsumeConversationState(bytesValue(f), m.Conversation)
}

func consumeActionMessage(f field, m *AgentRunRequest) error {
	if err := requireKind(f, protowire.BytesType, "cursor run request"); err != nil {
		return err
	}
	m.Action = new(ConversationAction)
	return ConsumeConversationAction(bytesValue(f), m.Action)
}

func consumeModelDetailsMessage(f field, m *AgentRunRequest) error {
	if err := requireKind(f, protowire.BytesType, "cursor run request"); err != nil {
		return err
	}
	m.ModelDetails = new(ModelDetails)
	return ConsumeModelDetails(bytesValue(f), m.ModelDetails)
}

func consumeMcpTools(f field, m *AgentRunRequest) error {
	if err := requireKind(f, protowire.BytesType, "cursor run request"); err != nil {
		return err
	}
	m.Tools = make([]McpToolDefinition, 0)
	return consumeFields(bytesValue(f), "cursor MCP tools", func(inner field) error {
		if inner.number != 1 {
			return nil
		}
		if err := requireKind(inner, protowire.BytesType, "cursor MCP tools"); err != nil {
			return err
		}
		var definition McpToolDefinition
		if err := ConsumeMcpToolDefinition(bytesValue(inner), &definition); err != nil {
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
		return appendMessage(dst, 1, m.Run, AppendAgentRunRequest)
	}
	if m.Exec != nil {
		return appendMessage(dst, 2, m.Exec, AppendExecClientMessage)
	}
	if m.Action != nil {
		return appendMessage(dst, 4, m.Action, AppendConversationAction)
	}
	if m.MessageNumber != 0 {
		return appendVariant(dst, protowire.Number(m.MessageNumber), m.MessageView)
	}
	return dst
}

func ConsumeAgentClientMessage(b []byte, m *AgentClientMessage) error {
	return consumeFields(b, "cursor client message", func(f field) error {
		if !isVariant(f, 1, 8) {
			return nil
		}
		*m = AgentClientMessage{}
		switch f.number {
		case 1:
			return consumeNested(f, "cursor client message", &m.Run, ConsumeAgentRunRequest)
		case 2:
			return consumeNested(f, "cursor client message", &m.Exec, ConsumeExecClientMessage)
		case 4:
			return consumeNested(f, "cursor client message", &m.Action, ConsumeConversationAction)
		}
		m.MessageNumber = f.number
		return setBytes(f, "cursor client message", &m.MessageView)
	})
}
