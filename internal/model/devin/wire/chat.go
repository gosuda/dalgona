package wire

import "google.golang.org/protobuf/encoding/protowire"

type ChatToolCall struct {
	IDView           []byte
	NameView         []byte
	ArgumentsView    []byte
	InvalidJSONView  []byte
	InvalidErrorView []byte
	Custom           bool
}

func AppendChatToolCall(dst []byte, m *ChatToolCall) []byte {
	dst = appendRaw(dst, 1, m.IDView)
	dst = appendRaw(dst, 2, m.NameView)
	dst = appendRaw(dst, 3, m.ArgumentsView)
	dst = appendRaw(dst, 4, m.InvalidJSONView)
	dst = appendRaw(dst, 5, m.InvalidErrorView)
	return appendBool(dst, 6, m.Custom)
}

func ConsumeChatToolCall(b []byte, m *ChatToolCall) error {
	return consumeFields(b, "devin tool call", func(f field) error {
		switch f.number {
		case 1:
			return setBytes(f, "devin tool call", &m.IDView)
		case 2:
			return setBytes(f, "devin tool call", &m.NameView)
		case 3:
			return setBytes(f, "devin tool call", &m.ArgumentsView)
		case 4:
			return setBytes(f, "devin tool call", &m.InvalidJSONView)
		case 5:
			return setBytes(f, "devin tool call", &m.InvalidErrorView)
		case 6:
			return setBool(f, "devin tool call", &m.Custom)
		default:
			return nil
		}
	})
}

type ChatMessagePrompt struct {
	MessageIDView        []byte
	Source               int32
	PromptView           []byte
	NumTokens            uint32
	SafeForCodeTelemetry bool
	ToolCalls            []ChatToolCall
	ToolCallIDView       []byte
	PromptCacheOptions   *PromptCacheOptions
	ToolResultIsError    bool
	Images               []ImageData
	ThinkingView         []byte
	SignatureView        []byte
	ThinkingRedacted     bool
	Annotations          [][]byte
	OutputIDView         []byte
	ThinkingIDView       []byte
	GeminiSignatureView  []byte
	SignatureTypeView    []byte
	PhaseView            []byte
}

func AppendChatMessagePrompt(dst []byte, m *ChatMessagePrompt) []byte {
	dst = appendRaw(dst, 1, m.MessageIDView)
	dst = appendInt32(dst, 2, m.Source)
	dst = appendRaw(dst, 3, m.PromptView)
	dst = appendUint32(dst, 4, m.NumTokens)
	dst = appendBool(dst, 5, m.SafeForCodeTelemetry)
	for i := range m.ToolCalls {
		dst = appendMessage(dst, 6, &m.ToolCalls[i], AppendChatToolCall)
	}
	dst = appendRaw(dst, 7, m.ToolCallIDView)
	if m.PromptCacheOptions != nil {
		dst = appendMessage(dst, 8, m.PromptCacheOptions, AppendPromptCacheOptions)
	}
	dst = appendBool(dst, 9, m.ToolResultIsError)
	for i := range m.Images {
		dst = appendMessage(dst, 10, &m.Images[i], AppendImageData)
	}
	dst = appendRaw(dst, 11, m.ThinkingView)
	dst = appendRaw(dst, 12, m.SignatureView)
	dst = appendBool(dst, 13, m.ThinkingRedacted)
	dst = appendRepeatedBytes(dst, 14, m.Annotations)
	dst = appendRaw(dst, 15, m.OutputIDView)
	dst = appendRaw(dst, 16, m.ThinkingIDView)
	dst = appendRaw(dst, 17, m.GeminiSignatureView)
	dst = appendRaw(dst, 18, m.SignatureTypeView)
	return appendRaw(dst, 19, m.PhaseView)
}

func ConsumeChatMessagePrompt(b []byte, m *ChatMessagePrompt) error {
	return consumeFields(b, "devin chat prompt", func(f field) error {
		switch {
		case f.number <= 5:
			return consumeChatPromptBase(f, m)
		case f.number <= 7:
			return consumeChatPromptTools(f, m)
		case f.number <= 13:
			return consumeChatPromptResult(f, m)
		default:
			return consumeChatPromptMetadata(f, m)
		}
	})
}

func consumeChatPromptBase(f field, m *ChatMessagePrompt) error {
	switch f.number {
	case 1:
		return setBytes(f, "devin chat prompt", &m.MessageIDView)
	case 2:
		return setInt32(f, "devin chat prompt", &m.Source)
	case 3:
		return setBytes(f, "devin chat prompt", &m.PromptView)
	case 4:
		return setUint32(f, "devin chat prompt", &m.NumTokens)
	case 5:
		return setBool(f, "devin chat prompt", &m.SafeForCodeTelemetry)
	}
	return nil
}

func consumeChatPromptTools(f field, m *ChatMessagePrompt) error {
	switch f.number {
	case 6:
		if err := requireKind(f, protowire.BytesType, "devin chat prompt"); err != nil {
			return err
		}
		var call ChatToolCall
		if err := ConsumeChatToolCall(bytesValue(f), &call); err != nil {
			return err
		}
		m.ToolCalls = append(m.ToolCalls, call)
	case 7:
		return setBytes(f, "devin chat prompt", &m.ToolCallIDView)
	}
	return nil
}

func consumeChatPromptResult(f field, m *ChatMessagePrompt) error {
	switch f.number {
	case 8:
		return consumeNested(f, "devin chat prompt", &m.PromptCacheOptions, ConsumePromptCacheOptions)
	case 9:
		return setBool(f, "devin chat prompt", &m.ToolResultIsError)
	case 10:
		return consumeRepeated(f, "devin chat prompt", &m.Images, ConsumeImageData)
	case 11:
		return setBytes(f, "devin chat prompt", &m.ThinkingView)
	case 12:
		return setBytes(f, "devin chat prompt", &m.SignatureView)
	case 13:
		return setBool(f, "devin chat prompt", &m.ThinkingRedacted)
	}
	return nil
}

func consumeChatPromptMetadata(f field, m *ChatMessagePrompt) error {
	switch f.number {
	case 14:
		return appendPromptBytes(f, &m.Annotations)
	case 15:
		return setBytes(f, "devin chat prompt", &m.OutputIDView)
	case 16:
		return setBytes(f, "devin chat prompt", &m.ThinkingIDView)
	case 17:
		return setBytes(f, "devin chat prompt", &m.GeminiSignatureView)
	case 18:
		return setBytes(f, "devin chat prompt", &m.SignatureTypeView)
	case 19:
		return setBytes(f, "devin chat prompt", &m.PhaseView)
	}
	return nil
}

func appendPromptBytes(f field, out *[][]byte) error {
	if err := requireKind(f, protowire.BytesType, "devin chat prompt"); err != nil {
		return err
	}
	*out = append(*out, bytesValue(f))
	return nil
}

type ChatToolDefinition struct {
	NameView                []byte
	DescriptionView         []byte
	JSONSchemaView          []byte
	AttributionFieldNames   [][]byte
	ServerNameView          []byte
	ReadOnlyHint            bool
	ComputerUseConfigView   []byte
	CustomTool              bool
	CustomToolGrammarView   []byte
	CustomGrammarSyntaxView []byte
	Strict                  bool
}

func AppendChatToolDefinition(dst []byte, m *ChatToolDefinition) []byte {
	dst = appendRaw(dst, 1, m.NameView)
	dst = appendRaw(dst, 2, m.DescriptionView)
	dst = appendRaw(dst, 3, m.JSONSchemaView)
	dst = appendRepeatedBytes(dst, 5, m.AttributionFieldNames)
	dst = appendRaw(dst, 6, m.ServerNameView)
	dst = appendBool(dst, 7, m.ReadOnlyHint)
	dst = appendRaw(dst, 8, m.ComputerUseConfigView)
	dst = appendBool(dst, 9, m.CustomTool)
	dst = appendRaw(dst, 10, m.CustomToolGrammarView)
	dst = appendRaw(dst, 11, m.CustomGrammarSyntaxView)
	return appendBool(dst, 12, m.Strict)
}

func ConsumeChatToolDefinition(b []byte, m *ChatToolDefinition) error {
	return consumeFields(b, "devin tool definition", func(f field) error {
		if f.number <= 6 {
			return consumeToolDefinitionBase(f, m)
		}
		return consumeToolDefinitionOptions(f, m)
	})
}

func consumeToolDefinitionBase(f field, m *ChatToolDefinition) error {
	switch f.number {
	case 1:
		return setBytes(f, "devin tool definition", &m.NameView)
	case 2:
		return setBytes(f, "devin tool definition", &m.DescriptionView)
	case 3:
		return setBytes(f, "devin tool definition", &m.JSONSchemaView)
	case 5:
		if err := requireKind(f, protowire.BytesType, "devin tool definition"); err != nil {
			return err
		}
		m.AttributionFieldNames = append(m.AttributionFieldNames, bytesValue(f))
	case 6:
		return setBytes(f, "devin tool definition", &m.ServerNameView)
	}
	return nil
}

func consumeToolDefinitionOptions(f field, m *ChatToolDefinition) error {
	switch f.number {
	case 7:
		return setBool(f, "devin tool definition", &m.ReadOnlyHint)
	case 8:
		return setBytes(f, "devin tool definition", &m.ComputerUseConfigView)
	case 9:
		return setBool(f, "devin tool definition", &m.CustomTool)
	case 10:
		return setBytes(f, "devin tool definition", &m.CustomToolGrammarView)
	case 11:
		return setBytes(f, "devin tool definition", &m.CustomGrammarSyntaxView)
	case 12:
		return setBool(f, "devin tool definition", &m.Strict)
	}
	return nil
}

type ChatToolChoice struct {
	ChoiceNumber int32
	ChoiceView   []byte
}

func AppendChatToolChoice(dst []byte, m *ChatToolChoice) []byte {
	if m.ChoiceNumber != 0 {
		return appendVariant(dst, protowire.Number(m.ChoiceNumber), m.ChoiceView)
	}
	return dst
}

func ConsumeChatToolChoice(b []byte, m *ChatToolChoice) error {
	return consumeFields(b, "devin tool choice", func(f field) error {
		if f.number != 1 && f.number != 2 {
			return nil
		}
		m.ChoiceNumber = f.number
		return setBytes(f, "devin tool choice", &m.ChoiceView)
	})
}

type GetChatMessageRequest struct {
	Metadata                *Metadata
	PromptView              []byte
	Prompts                 []ChatMessagePrompt
	UseInternalChatModel    bool
	InternalChatModel       int32
	RequestType             int32
	Configuration           *CompletionConfiguration
	ExperimentConfigView    []byte
	Tools                   []ChatToolDefinition
	DisableToolCalls        bool
	ToolChoice              *ChatToolChoice
	SystemPromptCache       *PromptCacheOptions
	ChatModelNameView       []byte
	TrajectoryReferenceView []byte
	ConversationIDView      []byte
	PromptIDView            []byte
	ProviderSource          int32
	Language                int32
	PlannerMode             int32
	ChatModelUIDView        []byte
	ExecutionIDView         []byte
	ArenaConvergeCount      *int32
	ArenaAssignmentJWTView  []byte
	ModelAssignmentJWTView  []byte
}

func AppendGetChatMessageRequest(dst []byte, m *GetChatMessageRequest) []byte {
	dst = appendOptionalMessage(dst, 1, m.Metadata, AppendMetadata)
	dst = appendRaw(dst, 2, m.PromptView)
	for i := range m.Prompts {
		dst = appendMessage(dst, 3, &m.Prompts[i], AppendChatMessagePrompt)
	}
	dst = appendBool(dst, 5, m.UseInternalChatModel)
	dst = appendInt32(dst, 6, m.InternalChatModel)
	dst = appendInt32(dst, 7, m.RequestType)
	dst = appendOptionalMessage(dst, 8, m.Configuration, AppendCompletionConfiguration)
	dst = appendRaw(dst, 9, m.ExperimentConfigView)
	for i := range m.Tools {
		dst = appendMessage(dst, 10, &m.Tools[i], AppendChatToolDefinition)
	}
	dst = appendBool(dst, 11, m.DisableToolCalls)
	dst = appendOptionalMessage(dst, 12, m.ToolChoice, AppendChatToolChoice)
	dst = appendOptionalMessage(dst, 13, m.SystemPromptCache, AppendPromptCacheOptions)
	dst = appendRaw(dst, 14, m.ChatModelNameView)
	dst = appendRaw(dst, 15, m.TrajectoryReferenceView)
	dst = appendRaw(dst, 16, m.ConversationIDView)
	dst = appendRaw(dst, 17, m.PromptIDView)
	dst = appendInt32(dst, 18, m.ProviderSource)
	dst = appendInt32(dst, 19, m.Language)
	dst = appendInt32(dst, 20, m.PlannerMode)
	dst = appendRaw(dst, 21, m.ChatModelUIDView)
	dst = appendRaw(dst, 22, m.ExecutionIDView)
	dst = appendPresentInt32(dst, 24, m.ArenaConvergeCount)
	dst = appendRaw(dst, 25, m.ArenaAssignmentJWTView)
	return appendRaw(dst, 26, m.ModelAssignmentJWTView)
}

func ConsumeGetChatMessageRequest(b []byte, m *GetChatMessageRequest) error {
	return consumeFields(b, "devin chat request", func(f field) error {
		switch {
		case f.number <= 5:
			return consumeChatRequestBase(f, m)
		case f.number <= 9:
			return consumeChatRequestConfiguration(f, m)
		case f.number <= 13:
			return consumeChatRequestTools(f, m)
		case f.number <= 16:
			return consumeChatRequestContext(f, m)
		case f.number <= 21:
			return consumeChatRequestRouting(f, m)
		default:
			return consumeChatRequestAssignment(f, m)
		}
	})
}

func consumeChatRequestBase(f field, m *GetChatMessageRequest) error {
	switch f.number {
	case 1:
		return consumeNested(f, "devin chat request", &m.Metadata, ConsumeMetadata)
	case 2:
		return setBytes(f, "devin chat request", &m.PromptView)
	case 3:
		if err := requireKind(f, protowire.BytesType, "devin chat request"); err != nil {
			return err
		}
		var prompt ChatMessagePrompt
		if err := ConsumeChatMessagePrompt(bytesValue(f), &prompt); err != nil {
			return err
		}
		m.Prompts = append(m.Prompts, prompt)
	case 5:
		return setBool(f, "devin chat request", &m.UseInternalChatModel)
	}
	return nil
}

func consumeChatRequestConfiguration(f field, m *GetChatMessageRequest) error {
	switch f.number {
	case 6:
		return setInt32(f, "devin chat request", &m.InternalChatModel)
	case 7:
		return setInt32(f, "devin chat request", &m.RequestType)
	case 8:
		return consumeNested(f, "devin chat request", &m.Configuration, ConsumeCompletionConfiguration)
	case 9:
		return setBytes(f, "devin chat request", &m.ExperimentConfigView)
	}
	return nil
}

func consumeChatRequestTools(f field, m *GetChatMessageRequest) error {
	switch f.number {
	case 10:
		if err := requireKind(f, protowire.BytesType, "devin chat request"); err != nil {
			return err
		}
		var definition ChatToolDefinition
		if err := ConsumeChatToolDefinition(bytesValue(f), &definition); err != nil {
			return err
		}
		m.Tools = append(m.Tools, definition)
	case 11:
		return setBool(f, "devin chat request", &m.DisableToolCalls)
	case 12:
		return consumeNested(f, "devin chat request", &m.ToolChoice, ConsumeChatToolChoice)
	case 13:
		return consumeNested(f, "devin chat request", &m.SystemPromptCache, ConsumePromptCacheOptions)
	}
	return nil
}

func consumeChatRequestContext(f field, m *GetChatMessageRequest) error {
	switch f.number {
	case 14:
		return setBytes(f, "devin chat request", &m.ChatModelNameView)
	case 15:
		return setBytes(f, "devin chat request", &m.TrajectoryReferenceView)
	case 16:
		return setBytes(f, "devin chat request", &m.ConversationIDView)
	}
	return nil
}

func consumeChatRequestRouting(f field, m *GetChatMessageRequest) error {
	switch f.number {
	case 17:
		return setBytes(f, "devin chat request", &m.PromptIDView)
	case 18:
		return setInt32(f, "devin chat request", &m.ProviderSource)
	case 19:
		return setInt32(f, "devin chat request", &m.Language)
	case 20:
		return setInt32(f, "devin chat request", &m.PlannerMode)
	case 21:
		return setBytes(f, "devin chat request", &m.ChatModelUIDView)
	}
	return nil
}

func consumeChatRequestAssignment(f field, m *GetChatMessageRequest) error {
	switch f.number {
	case 22:
		return setBytes(f, "devin chat request", &m.ExecutionIDView)
	case 24:
		return setPresentInt32(f, "devin chat request", &m.ArenaConvergeCount)
	case 25:
		return setBytes(f, "devin chat request", &m.ArenaAssignmentJWTView)
	case 26:
		return setBytes(f, "devin chat request", &m.ModelAssignmentJWTView)
	}
	return nil
}

type ResponseHeader struct {
	NameView  []byte
	ValueView []byte
}

func AppendResponseHeader(dst []byte, m *ResponseHeader) []byte {
	dst = appendRaw(dst, 1, m.NameView)
	return appendRaw(dst, 2, m.ValueView)
}

func ConsumeResponseHeader(b []byte, m *ResponseHeader) error {
	return consumeFields(b, "devin response header", func(f field) error {
		switch f.number {
		case 1:
			return setBytes(f, "devin response header", &m.NameView)
		case 2:
			return setBytes(f, "devin response header", &m.ValueView)
		default:
			return nil
		}
	})
}

type ModelUsageStats struct {
	ModelDeprecated  int32
	Input            uint64
	Output           uint64
	CacheWrite       uint64
	CacheRead        uint64
	APIProvider      int32
	MessageIDView    []byte
	ResponseHeaders  []ResponseHeader
	ModelUIDView     []byte
	BillingUIDView   []byte
	RequestedUIDView []byte
}

func AppendModelUsageStats(dst []byte, m *ModelUsageStats) []byte {
	dst = appendInt32(dst, 1, m.ModelDeprecated)
	dst = appendUint64(dst, 2, m.Input)
	dst = appendUint64(dst, 3, m.Output)
	dst = appendUint64(dst, 4, m.CacheWrite)
	dst = appendUint64(dst, 5, m.CacheRead)
	dst = appendInt32(dst, 6, m.APIProvider)
	dst = appendRaw(dst, 7, m.MessageIDView)
	for i := range m.ResponseHeaders {
		dst = appendMessage(dst, 8, &m.ResponseHeaders[i], AppendResponseHeader)
	}
	dst = appendRaw(dst, 9, m.ModelUIDView)
	dst = appendRaw(dst, 10, m.BillingUIDView)
	return appendRaw(dst, 11, m.RequestedUIDView)
}

func ConsumeModelUsageStats(b []byte, m *ModelUsageStats) error {
	return consumeFields(b, "devin usage", func(f field) error {
		if f.number <= 6 {
			return consumeUsageNumbers(f, m)
		}
		return consumeUsageNames(f, m)
	})
}

func consumeUsageNumbers(f field, m *ModelUsageStats) error {
	switch f.number {
	case 1:
		return setInt32(f, "devin usage", &m.ModelDeprecated)
	case 2:
		return setUint64(f, "devin usage", &m.Input)
	case 3:
		return setUint64(f, "devin usage", &m.Output)
	case 4:
		return setUint64(f, "devin usage", &m.CacheWrite)
	case 5:
		return setUint64(f, "devin usage", &m.CacheRead)
	case 6:
		return setInt32(f, "devin usage", &m.APIProvider)
	}
	return nil
}

func consumeUsageNames(f field, m *ModelUsageStats) error {
	switch f.number {
	case 7:
		return setBytes(f, "devin usage", &m.MessageIDView)
	case 8:
		if err := requireKind(f, protowire.BytesType, "devin usage"); err != nil {
			return err
		}
		var header ResponseHeader
		if err := ConsumeResponseHeader(bytesValue(f), &header); err != nil {
			return err
		}
		m.ResponseHeaders = append(m.ResponseHeaders, header)
	case 9:
		return setBytes(f, "devin usage", &m.ModelUIDView)
	case 10:
		return setBytes(f, "devin usage", &m.BillingUIDView)
	case 11:
		return setBytes(f, "devin usage", &m.RequestedUIDView)
	}
	return nil
}

type GetChatMessageResponse struct {
	MessageIDView             []byte
	Timestamp                 *Timestamp
	DeltaTextView             []byte
	DeltaTokens               uint32
	StopReason                int32
	ToolCalls                 []ChatToolCall
	Usage                     *ModelUsageStats
	Redact                    bool
	DeltaThinkingView         []byte
	DeltaSignatureView        []byte
	ThinkingRedacted          bool
	Latency                   float64
	CompletionProfileView     []byte
	CreditCost                int32
	OutputIDView              []byte
	ThinkingIDView            []byte
	RequestIDView             []byte
	CommittedCreditCost       int32
	PromptView                []byte
	GeminiSignatureView       []byte
	SignatureTypeView         []byte
	CommittedACUCost          float64
	ActualModelUIDView        []byte
	ArenaInvocationCapReached bool
	PhaseView                 []byte
	CommittedQuotaCost        int64
	CommittedOverageCost      int64
	ResponseDimensionGroups   [][]byte
}

func AppendGetChatMessageResponse(dst []byte, m *GetChatMessageResponse) []byte {
	dst = appendRaw(dst, 1, m.MessageIDView)
	dst = appendOptionalMessage(dst, 2, m.Timestamp, AppendTimestamp)
	dst = appendRaw(dst, 3, m.DeltaTextView)
	dst = appendUint32(dst, 4, m.DeltaTokens)
	dst = appendInt32(dst, 5, m.StopReason)
	for i := range m.ToolCalls {
		dst = appendMessage(dst, 6, &m.ToolCalls[i], AppendChatToolCall)
	}
	if m.Usage != nil {
		dst = appendMessage(dst, 7, m.Usage, AppendModelUsageStats)
	}
	dst = appendBool(dst, 8, m.Redact)
	dst = appendRaw(dst, 9, m.DeltaThinkingView)
	dst = appendRaw(dst, 10, m.DeltaSignatureView)
	dst = appendBool(dst, 11, m.ThinkingRedacted)
	dst = appendFloat64(dst, 12, m.Latency)
	dst = appendRaw(dst, 13, m.CompletionProfileView)
	dst = appendInt32(dst, 14, m.CreditCost)
	dst = appendRaw(dst, 15, m.OutputIDView)
	dst = appendRaw(dst, 16, m.ThinkingIDView)
	dst = appendRaw(dst, 17, m.RequestIDView)
	dst = appendInt32(dst, 18, m.CommittedCreditCost)
	dst = appendRaw(dst, 19, m.PromptView)
	dst = appendRaw(dst, 20, m.GeminiSignatureView)
	dst = appendRaw(dst, 21, m.SignatureTypeView)
	dst = appendFloat64(dst, 22, m.CommittedACUCost)
	dst = appendRaw(dst, 23, m.ActualModelUIDView)
	dst = appendBool(dst, 24, m.ArenaInvocationCapReached)
	dst = appendRaw(dst, 25, m.PhaseView)
	dst = appendInt64(dst, 26, m.CommittedQuotaCost)
	dst = appendInt64(dst, 27, m.CommittedOverageCost)
	return appendRepeatedBytes(dst, 28, m.ResponseDimensionGroups)
}

func ConsumeGetChatMessageResponse(b []byte, m *GetChatMessageResponse) error {
	return consumeFields(b, "devin chat response", func(f field) error {
		switch {
		case f.number <= 5:
			return consumeChatResponseBase(f, m)
		case f.number <= 7:
			return consumeChatResponseTools(f, m)
		case f.number <= 13:
			return consumeChatResponseThinking(f, m)
		case f.number <= 21:
			return consumeChatResponseBilling(f, m)
		default:
			return consumeChatResponseRouting(f, m)
		}
	})
}

func consumeChatResponseBase(f field, m *GetChatMessageResponse) error {
	switch f.number {
	case 1:
		return setBytes(f, "devin chat response", &m.MessageIDView)
	case 2:
		return consumeNested(f, "devin chat response", &m.Timestamp, ConsumeTimestamp)
	case 3:
		return setBytes(f, "devin chat response", &m.DeltaTextView)
	case 4:
		return setUint32(f, "devin chat response", &m.DeltaTokens)
	case 5:
		return setInt32(f, "devin chat response", &m.StopReason)
	}
	return nil
}

func consumeChatResponseTools(f field, m *GetChatMessageResponse) error {
	switch f.number {
	case 6:
		if err := requireKind(f, protowire.BytesType, "devin chat response"); err != nil {
			return err
		}
		var call ChatToolCall
		if err := ConsumeChatToolCall(bytesValue(f), &call); err != nil {
			return err
		}
		m.ToolCalls = append(m.ToolCalls, call)
	case 7:
		if err := requireKind(f, protowire.BytesType, "devin chat response"); err != nil {
			return err
		}
		m.Usage = new(ModelUsageStats)
		return ConsumeModelUsageStats(bytesValue(f), m.Usage)
	}
	return nil
}

func consumeChatResponseThinking(f field, m *GetChatMessageResponse) error {
	switch f.number {
	case 8:
		return setBool(f, "devin chat response", &m.Redact)
	case 9:
		return setBytes(f, "devin chat response", &m.DeltaThinkingView)
	case 10:
		return setBytes(f, "devin chat response", &m.DeltaSignatureView)
	case 11:
		return setBool(f, "devin chat response", &m.ThinkingRedacted)
	case 12:
		return setFloat64(f, "devin chat response", &m.Latency)
	case 13:
		return setBytes(f, "devin chat response", &m.CompletionProfileView)
	}
	return nil
}

func consumeChatResponseBilling(f field, m *GetChatMessageResponse) error {
	switch f.number {
	case 14:
		return setInt32(f, "devin chat response", &m.CreditCost)
	case 15:
		return setBytes(f, "devin chat response", &m.OutputIDView)
	case 16:
		return setBytes(f, "devin chat response", &m.ThinkingIDView)
	case 17:
		return setBytes(f, "devin chat response", &m.RequestIDView)
	case 18:
		return setInt32(f, "devin chat response", &m.CommittedCreditCost)
	case 19:
		return setBytes(f, "devin chat response", &m.PromptView)
	case 20:
		return setBytes(f, "devin chat response", &m.GeminiSignatureView)
	case 21:
		return setBytes(f, "devin chat response", &m.SignatureTypeView)
	}
	return nil
}

func consumeChatResponseRouting(f field, m *GetChatMessageResponse) error {
	switch f.number {
	case 22:
		return setFloat64(f, "devin chat response", &m.CommittedACUCost)
	case 23:
		return setBytes(f, "devin chat response", &m.ActualModelUIDView)
	case 24:
		return setBool(f, "devin chat response", &m.ArenaInvocationCapReached)
	case 25:
		return setBytes(f, "devin chat response", &m.PhaseView)
	case 26:
		return setInt64(f, "devin chat response", &m.CommittedQuotaCost)
	case 27:
		return setInt64(f, "devin chat response", &m.CommittedOverageCost)
	case 28:
		if err := requireKind(f, protowire.BytesType, "devin chat response"); err != nil {
			return err
		}
		m.ResponseDimensionGroups = append(m.ResponseDimensionGroups, bytesValue(f))
	}
	return nil
}
