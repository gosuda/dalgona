package wire

import (
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"google.golang.org/protobuf/encoding/protowire"
)

type ChatToolCall struct {
	IDView           []byte
	NameView         []byte
	ArgumentsView    []byte
	InvalidJSONView  []byte
	InvalidErrorView []byte
	Custom           bool
}

func AppendChatToolCall(dst []byte, m *ChatToolCall) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.IDView)
	dst = connectwire.AppendRaw(dst, 2, m.NameView)
	dst = connectwire.AppendRaw(dst, 3, m.ArgumentsView)
	dst = connectwire.AppendRaw(dst, 4, m.InvalidJSONView)
	dst = connectwire.AppendRaw(dst, 5, m.InvalidErrorView)
	return connectwire.AppendBool(dst, 6, m.Custom)
}

func ConsumeChatToolCall(b []byte, m *ChatToolCall) error {
	return connectwire.ConsumeFields(b, "devin tool call", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "devin tool call", &m.IDView)
		case 2:
			return connectwire.SetBytes(f, "devin tool call", &m.NameView)
		case 3:
			return connectwire.SetBytes(f, "devin tool call", &m.ArgumentsView)
		case 4:
			return connectwire.SetBytes(f, "devin tool call", &m.InvalidJSONView)
		case 5:
			return connectwire.SetBytes(f, "devin tool call", &m.InvalidErrorView)
		case 6:
			return connectwire.SetBool(f, "devin tool call", &m.Custom)
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
	dst = connectwire.AppendRaw(dst, 1, m.MessageIDView)
	dst = connectwire.AppendInt32(dst, 2, m.Source)
	dst = connectwire.AppendRaw(dst, 3, m.PromptView)
	dst = connectwire.AppendUint32(dst, 4, m.NumTokens)
	dst = connectwire.AppendBool(dst, 5, m.SafeForCodeTelemetry)
	for i := range m.ToolCalls {
		dst = connectwire.AppendMessage(dst, 6, &m.ToolCalls[i], AppendChatToolCall)
	}
	dst = connectwire.AppendRaw(dst, 7, m.ToolCallIDView)
	if m.PromptCacheOptions != nil {
		dst = connectwire.AppendMessage(dst, 8, m.PromptCacheOptions, AppendPromptCacheOptions)
	}
	dst = connectwire.AppendBool(dst, 9, m.ToolResultIsError)
	for i := range m.Images {
		dst = connectwire.AppendMessage(dst, 10, &m.Images[i], AppendImageData)
	}
	dst = connectwire.AppendRaw(dst, 11, m.ThinkingView)
	dst = connectwire.AppendRaw(dst, 12, m.SignatureView)
	dst = connectwire.AppendBool(dst, 13, m.ThinkingRedacted)
	dst = connectwire.AppendRepeatedBytes(dst, 14, m.Annotations)
	dst = connectwire.AppendRaw(dst, 15, m.OutputIDView)
	dst = connectwire.AppendRaw(dst, 16, m.ThinkingIDView)
	dst = connectwire.AppendRaw(dst, 17, m.GeminiSignatureView)
	dst = connectwire.AppendRaw(dst, 18, m.SignatureTypeView)
	return connectwire.AppendRaw(dst, 19, m.PhaseView)
}

func ConsumeChatMessagePrompt(b []byte, m *ChatMessagePrompt) error {
	return connectwire.ConsumeFields(b, "devin chat prompt", func(f connectwire.Field) error {
		switch {
		case f.Number <= 5:
			return consumeChatPromptBase(f, m)
		case f.Number <= 7:
			return consumeChatPromptTools(f, m)
		case f.Number <= 13:
			return consumeChatPromptResult(f, m)
		default:
			return consumeChatPromptMetadata(f, m)
		}
	})
}

func consumeChatPromptBase(f connectwire.Field, m *ChatMessagePrompt) error {
	switch f.Number {
	case 1:
		return connectwire.SetBytes(f, "devin chat prompt", &m.MessageIDView)
	case 2:
		return connectwire.SetInt32(f, "devin chat prompt", &m.Source)
	case 3:
		return connectwire.SetBytes(f, "devin chat prompt", &m.PromptView)
	case 4:
		return connectwire.SetUint32(f, "devin chat prompt", &m.NumTokens)
	case 5:
		return connectwire.SetBool(f, "devin chat prompt", &m.SafeForCodeTelemetry)
	}
	return nil
}

func consumeChatPromptTools(f connectwire.Field, m *ChatMessagePrompt) error {
	switch f.Number {
	case 6:
		if err := connectwire.RequireKind(f, protowire.BytesType, "devin chat prompt"); err != nil {
			return err
		}
		var call ChatToolCall
		if err := ConsumeChatToolCall(connectwire.BytesValue(f), &call); err != nil {
			return err
		}
		m.ToolCalls = append(m.ToolCalls, call)
	case 7:
		return connectwire.SetBytes(f, "devin chat prompt", &m.ToolCallIDView)
	}
	return nil
}

func consumeChatPromptResult(f connectwire.Field, m *ChatMessagePrompt) error {
	switch f.Number {
	case 8:
		return connectwire.ConsumeNested(f, "devin chat prompt", &m.PromptCacheOptions, ConsumePromptCacheOptions)
	case 9:
		return connectwire.SetBool(f, "devin chat prompt", &m.ToolResultIsError)
	case 10:
		return connectwire.ConsumeRepeated(f, "devin chat prompt", &m.Images, ConsumeImageData)
	case 11:
		return connectwire.SetBytes(f, "devin chat prompt", &m.ThinkingView)
	case 12:
		return connectwire.SetBytes(f, "devin chat prompt", &m.SignatureView)
	case 13:
		return connectwire.SetBool(f, "devin chat prompt", &m.ThinkingRedacted)
	}
	return nil
}

func consumeChatPromptMetadata(f connectwire.Field, m *ChatMessagePrompt) error {
	switch f.Number {
	case 14:
		return appendPromptBytes(f, &m.Annotations)
	case 15:
		return connectwire.SetBytes(f, "devin chat prompt", &m.OutputIDView)
	case 16:
		return connectwire.SetBytes(f, "devin chat prompt", &m.ThinkingIDView)
	case 17:
		return connectwire.SetBytes(f, "devin chat prompt", &m.GeminiSignatureView)
	case 18:
		return connectwire.SetBytes(f, "devin chat prompt", &m.SignatureTypeView)
	case 19:
		return connectwire.SetBytes(f, "devin chat prompt", &m.PhaseView)
	}
	return nil
}

func appendPromptBytes(f connectwire.Field, out *[][]byte) error {
	if err := connectwire.RequireKind(f, protowire.BytesType, "devin chat prompt"); err != nil {
		return err
	}
	*out = append(*out, connectwire.BytesValue(f))
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
	dst = connectwire.AppendRaw(dst, 1, m.NameView)
	dst = connectwire.AppendRaw(dst, 2, m.DescriptionView)
	dst = connectwire.AppendRaw(dst, 3, m.JSONSchemaView)
	dst = connectwire.AppendRepeatedBytes(dst, 5, m.AttributionFieldNames)
	dst = connectwire.AppendRaw(dst, 6, m.ServerNameView)
	dst = connectwire.AppendBool(dst, 7, m.ReadOnlyHint)
	dst = connectwire.AppendRaw(dst, 8, m.ComputerUseConfigView)
	dst = connectwire.AppendBool(dst, 9, m.CustomTool)
	dst = connectwire.AppendRaw(dst, 10, m.CustomToolGrammarView)
	dst = connectwire.AppendRaw(dst, 11, m.CustomGrammarSyntaxView)
	return connectwire.AppendBool(dst, 12, m.Strict)
}

func ConsumeChatToolDefinition(b []byte, m *ChatToolDefinition) error {
	return connectwire.ConsumeFields(b, "devin tool definition", func(f connectwire.Field) error {
		if f.Number <= 6 {
			return consumeToolDefinitionBase(f, m)
		}
		return consumeToolDefinitionOptions(f, m)
	})
}

func consumeToolDefinitionBase(f connectwire.Field, m *ChatToolDefinition) error {
	switch f.Number {
	case 1:
		return connectwire.SetBytes(f, "devin tool definition", &m.NameView)
	case 2:
		return connectwire.SetBytes(f, "devin tool definition", &m.DescriptionView)
	case 3:
		return connectwire.SetBytes(f, "devin tool definition", &m.JSONSchemaView)
	case 5:
		if err := connectwire.RequireKind(f, protowire.BytesType, "devin tool definition"); err != nil {
			return err
		}
		m.AttributionFieldNames = append(m.AttributionFieldNames, connectwire.BytesValue(f))
	case 6:
		return connectwire.SetBytes(f, "devin tool definition", &m.ServerNameView)
	}
	return nil
}

func consumeToolDefinitionOptions(f connectwire.Field, m *ChatToolDefinition) error {
	switch f.Number {
	case 7:
		return connectwire.SetBool(f, "devin tool definition", &m.ReadOnlyHint)
	case 8:
		return connectwire.SetBytes(f, "devin tool definition", &m.ComputerUseConfigView)
	case 9:
		return connectwire.SetBool(f, "devin tool definition", &m.CustomTool)
	case 10:
		return connectwire.SetBytes(f, "devin tool definition", &m.CustomToolGrammarView)
	case 11:
		return connectwire.SetBytes(f, "devin tool definition", &m.CustomGrammarSyntaxView)
	case 12:
		return connectwire.SetBool(f, "devin tool definition", &m.Strict)
	}
	return nil
}

type ChatToolChoice struct {
	ChoiceNumber int32
	ChoiceView   []byte
}

func AppendChatToolChoice(dst []byte, m *ChatToolChoice) []byte {
	if m.ChoiceNumber != 0 {
		return connectwire.AppendVariant(dst, protowire.Number(m.ChoiceNumber), m.ChoiceView)
	}
	return dst
}

func ConsumeChatToolChoice(b []byte, m *ChatToolChoice) error {
	return connectwire.ConsumeFields(b, "devin tool choice", func(f connectwire.Field) error {
		if f.Number != 1 && f.Number != 2 {
			return nil
		}
		m.ChoiceNumber = f.Number
		return connectwire.SetBytes(f, "devin tool choice", &m.ChoiceView)
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
	dst = connectwire.AppendOptionalMessage(dst, 1, m.Metadata, AppendMetadata)
	dst = connectwire.AppendRaw(dst, 2, m.PromptView)
	for i := range m.Prompts {
		dst = connectwire.AppendMessage(dst, 3, &m.Prompts[i], AppendChatMessagePrompt)
	}
	dst = connectwire.AppendBool(dst, 5, m.UseInternalChatModel)
	dst = connectwire.AppendInt32(dst, 6, m.InternalChatModel)
	dst = connectwire.AppendInt32(dst, 7, m.RequestType)
	dst = connectwire.AppendOptionalMessage(dst, 8, m.Configuration, AppendCompletionConfiguration)
	dst = connectwire.AppendRaw(dst, 9, m.ExperimentConfigView)
	for i := range m.Tools {
		dst = connectwire.AppendMessage(dst, 10, &m.Tools[i], AppendChatToolDefinition)
	}
	dst = connectwire.AppendBool(dst, 11, m.DisableToolCalls)
	dst = connectwire.AppendOptionalMessage(dst, 12, m.ToolChoice, AppendChatToolChoice)
	dst = connectwire.AppendOptionalMessage(dst, 13, m.SystemPromptCache, AppendPromptCacheOptions)
	dst = connectwire.AppendRaw(dst, 14, m.ChatModelNameView)
	dst = connectwire.AppendRaw(dst, 15, m.TrajectoryReferenceView)
	dst = connectwire.AppendRaw(dst, 16, m.ConversationIDView)
	dst = connectwire.AppendRaw(dst, 17, m.PromptIDView)
	dst = connectwire.AppendInt32(dst, 18, m.ProviderSource)
	dst = connectwire.AppendInt32(dst, 19, m.Language)
	dst = connectwire.AppendInt32(dst, 20, m.PlannerMode)
	dst = connectwire.AppendRaw(dst, 21, m.ChatModelUIDView)
	dst = connectwire.AppendRaw(dst, 22, m.ExecutionIDView)
	dst = appendPresentInt32(dst, 24, m.ArenaConvergeCount)
	dst = connectwire.AppendRaw(dst, 25, m.ArenaAssignmentJWTView)
	return connectwire.AppendRaw(dst, 26, m.ModelAssignmentJWTView)
}

func ConsumeGetChatMessageRequest(b []byte, m *GetChatMessageRequest) error {
	return connectwire.ConsumeFields(b, "devin chat request", func(f connectwire.Field) error {
		switch {
		case f.Number <= 5:
			return consumeChatRequestBase(f, m)
		case f.Number <= 9:
			return consumeChatRequestConfiguration(f, m)
		case f.Number <= 13:
			return consumeChatRequestTools(f, m)
		case f.Number <= 16:
			return consumeChatRequestContext(f, m)
		case f.Number <= 21:
			return consumeChatRequestRouting(f, m)
		default:
			return consumeChatRequestAssignment(f, m)
		}
	})
}

func consumeChatRequestBase(f connectwire.Field, m *GetChatMessageRequest) error {
	switch f.Number {
	case 1:
		return connectwire.ConsumeNested(f, "devin chat request", &m.Metadata, ConsumeMetadata)
	case 2:
		return connectwire.SetBytes(f, "devin chat request", &m.PromptView)
	case 3:
		if err := connectwire.RequireKind(f, protowire.BytesType, "devin chat request"); err != nil {
			return err
		}
		var prompt ChatMessagePrompt
		if err := ConsumeChatMessagePrompt(connectwire.BytesValue(f), &prompt); err != nil {
			return err
		}
		m.Prompts = append(m.Prompts, prompt)
	case 5:
		return connectwire.SetBool(f, "devin chat request", &m.UseInternalChatModel)
	}
	return nil
}

func consumeChatRequestConfiguration(f connectwire.Field, m *GetChatMessageRequest) error {
	switch f.Number {
	case 6:
		return connectwire.SetInt32(f, "devin chat request", &m.InternalChatModel)
	case 7:
		return connectwire.SetInt32(f, "devin chat request", &m.RequestType)
	case 8:
		return connectwire.ConsumeNested(f, "devin chat request", &m.Configuration, ConsumeCompletionConfiguration)
	case 9:
		return connectwire.SetBytes(f, "devin chat request", &m.ExperimentConfigView)
	}
	return nil
}

func consumeChatRequestTools(f connectwire.Field, m *GetChatMessageRequest) error {
	switch f.Number {
	case 10:
		if err := connectwire.RequireKind(f, protowire.BytesType, "devin chat request"); err != nil {
			return err
		}
		var definition ChatToolDefinition
		if err := ConsumeChatToolDefinition(connectwire.BytesValue(f), &definition); err != nil {
			return err
		}
		m.Tools = append(m.Tools, definition)
	case 11:
		return connectwire.SetBool(f, "devin chat request", &m.DisableToolCalls)
	case 12:
		return connectwire.ConsumeNested(f, "devin chat request", &m.ToolChoice, ConsumeChatToolChoice)
	case 13:
		return connectwire.ConsumeNested(f, "devin chat request", &m.SystemPromptCache, ConsumePromptCacheOptions)
	}
	return nil
}

func consumeChatRequestContext(f connectwire.Field, m *GetChatMessageRequest) error {
	switch f.Number {
	case 14:
		return connectwire.SetBytes(f, "devin chat request", &m.ChatModelNameView)
	case 15:
		return connectwire.SetBytes(f, "devin chat request", &m.TrajectoryReferenceView)
	case 16:
		return connectwire.SetBytes(f, "devin chat request", &m.ConversationIDView)
	}
	return nil
}

func consumeChatRequestRouting(f connectwire.Field, m *GetChatMessageRequest) error {
	switch f.Number {
	case 17:
		return connectwire.SetBytes(f, "devin chat request", &m.PromptIDView)
	case 18:
		return connectwire.SetInt32(f, "devin chat request", &m.ProviderSource)
	case 19:
		return connectwire.SetInt32(f, "devin chat request", &m.Language)
	case 20:
		return connectwire.SetInt32(f, "devin chat request", &m.PlannerMode)
	case 21:
		return connectwire.SetBytes(f, "devin chat request", &m.ChatModelUIDView)
	}
	return nil
}

func consumeChatRequestAssignment(f connectwire.Field, m *GetChatMessageRequest) error {
	switch f.Number {
	case 22:
		return connectwire.SetBytes(f, "devin chat request", &m.ExecutionIDView)
	case 24:
		return setPresentInt32(f, "devin chat request", &m.ArenaConvergeCount)
	case 25:
		return connectwire.SetBytes(f, "devin chat request", &m.ArenaAssignmentJWTView)
	case 26:
		return connectwire.SetBytes(f, "devin chat request", &m.ModelAssignmentJWTView)
	}
	return nil
}

type ResponseHeader struct {
	NameView  []byte
	ValueView []byte
}

func AppendResponseHeader(dst []byte, m *ResponseHeader) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.NameView)
	return connectwire.AppendRaw(dst, 2, m.ValueView)
}

func ConsumeResponseHeader(b []byte, m *ResponseHeader) error {
	return connectwire.ConsumeFields(b, "devin response header", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "devin response header", &m.NameView)
		case 2:
			return connectwire.SetBytes(f, "devin response header", &m.ValueView)
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
	dst = connectwire.AppendInt32(dst, 1, m.ModelDeprecated)
	dst = connectwire.AppendUint64(dst, 2, m.Input)
	dst = connectwire.AppendUint64(dst, 3, m.Output)
	dst = connectwire.AppendUint64(dst, 4, m.CacheWrite)
	dst = connectwire.AppendUint64(dst, 5, m.CacheRead)
	dst = connectwire.AppendInt32(dst, 6, m.APIProvider)
	dst = connectwire.AppendRaw(dst, 7, m.MessageIDView)
	for i := range m.ResponseHeaders {
		dst = connectwire.AppendMessage(dst, 8, &m.ResponseHeaders[i], AppendResponseHeader)
	}
	dst = connectwire.AppendRaw(dst, 9, m.ModelUIDView)
	dst = connectwire.AppendRaw(dst, 10, m.BillingUIDView)
	return connectwire.AppendRaw(dst, 11, m.RequestedUIDView)
}

func ConsumeModelUsageStats(b []byte, m *ModelUsageStats) error {
	return connectwire.ConsumeFields(b, "devin usage", func(f connectwire.Field) error {
		if f.Number <= 6 {
			return consumeUsageNumbers(f, m)
		}
		return consumeUsageNames(f, m)
	})
}

func consumeUsageNumbers(f connectwire.Field, m *ModelUsageStats) error {
	switch f.Number {
	case 1:
		return connectwire.SetInt32(f, "devin usage", &m.ModelDeprecated)
	case 2:
		return connectwire.SetUint64(f, "devin usage", &m.Input)
	case 3:
		return connectwire.SetUint64(f, "devin usage", &m.Output)
	case 4:
		return connectwire.SetUint64(f, "devin usage", &m.CacheWrite)
	case 5:
		return connectwire.SetUint64(f, "devin usage", &m.CacheRead)
	case 6:
		return connectwire.SetInt32(f, "devin usage", &m.APIProvider)
	}
	return nil
}

func consumeUsageNames(f connectwire.Field, m *ModelUsageStats) error {
	switch f.Number {
	case 7:
		return connectwire.SetBytes(f, "devin usage", &m.MessageIDView)
	case 8:
		if err := connectwire.RequireKind(f, protowire.BytesType, "devin usage"); err != nil {
			return err
		}
		var header ResponseHeader
		if err := ConsumeResponseHeader(connectwire.BytesValue(f), &header); err != nil {
			return err
		}
		m.ResponseHeaders = append(m.ResponseHeaders, header)
	case 9:
		return connectwire.SetBytes(f, "devin usage", &m.ModelUIDView)
	case 10:
		return connectwire.SetBytes(f, "devin usage", &m.BillingUIDView)
	case 11:
		return connectwire.SetBytes(f, "devin usage", &m.RequestedUIDView)
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
	dst = connectwire.AppendRaw(dst, 1, m.MessageIDView)
	dst = connectwire.AppendOptionalMessage(dst, 2, m.Timestamp, AppendTimestamp)
	dst = connectwire.AppendRaw(dst, 3, m.DeltaTextView)
	dst = connectwire.AppendUint32(dst, 4, m.DeltaTokens)
	dst = connectwire.AppendInt32(dst, 5, m.StopReason)
	for i := range m.ToolCalls {
		dst = connectwire.AppendMessage(dst, 6, &m.ToolCalls[i], AppendChatToolCall)
	}
	if m.Usage != nil {
		dst = connectwire.AppendMessage(dst, 7, m.Usage, AppendModelUsageStats)
	}
	dst = connectwire.AppendBool(dst, 8, m.Redact)
	dst = connectwire.AppendRaw(dst, 9, m.DeltaThinkingView)
	dst = connectwire.AppendRaw(dst, 10, m.DeltaSignatureView)
	dst = connectwire.AppendBool(dst, 11, m.ThinkingRedacted)
	dst = appendFloat64(dst, 12, m.Latency)
	dst = connectwire.AppendRaw(dst, 13, m.CompletionProfileView)
	dst = connectwire.AppendInt32(dst, 14, m.CreditCost)
	dst = connectwire.AppendRaw(dst, 15, m.OutputIDView)
	dst = connectwire.AppendRaw(dst, 16, m.ThinkingIDView)
	dst = connectwire.AppendRaw(dst, 17, m.RequestIDView)
	dst = connectwire.AppendInt32(dst, 18, m.CommittedCreditCost)
	dst = connectwire.AppendRaw(dst, 19, m.PromptView)
	dst = connectwire.AppendRaw(dst, 20, m.GeminiSignatureView)
	dst = connectwire.AppendRaw(dst, 21, m.SignatureTypeView)
	dst = appendFloat64(dst, 22, m.CommittedACUCost)
	dst = connectwire.AppendRaw(dst, 23, m.ActualModelUIDView)
	dst = connectwire.AppendBool(dst, 24, m.ArenaInvocationCapReached)
	dst = connectwire.AppendRaw(dst, 25, m.PhaseView)
	dst = connectwire.AppendInt64(dst, 26, m.CommittedQuotaCost)
	dst = connectwire.AppendInt64(dst, 27, m.CommittedOverageCost)
	return connectwire.AppendRepeatedBytes(dst, 28, m.ResponseDimensionGroups)
}

func ConsumeGetChatMessageResponse(b []byte, m *GetChatMessageResponse) error {
	return connectwire.ConsumeFields(b, "devin chat response", func(f connectwire.Field) error {
		switch {
		case f.Number <= 5:
			return consumeChatResponseBase(f, m)
		case f.Number <= 7:
			return consumeChatResponseTools(f, m)
		case f.Number <= 13:
			return consumeChatResponseThinking(f, m)
		case f.Number <= 21:
			return consumeChatResponseBilling(f, m)
		default:
			return consumeChatResponseRouting(f, m)
		}
	})
}

func consumeChatResponseBase(f connectwire.Field, m *GetChatMessageResponse) error {
	switch f.Number {
	case 1:
		return connectwire.SetBytes(f, "devin chat response", &m.MessageIDView)
	case 2:
		return connectwire.ConsumeNested(f, "devin chat response", &m.Timestamp, ConsumeTimestamp)
	case 3:
		return connectwire.SetBytes(f, "devin chat response", &m.DeltaTextView)
	case 4:
		return connectwire.SetUint32(f, "devin chat response", &m.DeltaTokens)
	case 5:
		return connectwire.SetInt32(f, "devin chat response", &m.StopReason)
	}
	return nil
}

func consumeChatResponseTools(f connectwire.Field, m *GetChatMessageResponse) error {
	switch f.Number {
	case 6:
		if err := connectwire.RequireKind(f, protowire.BytesType, "devin chat response"); err != nil {
			return err
		}
		var call ChatToolCall
		if err := ConsumeChatToolCall(connectwire.BytesValue(f), &call); err != nil {
			return err
		}
		m.ToolCalls = append(m.ToolCalls, call)
	case 7:
		if err := connectwire.RequireKind(f, protowire.BytesType, "devin chat response"); err != nil {
			return err
		}
		m.Usage = new(ModelUsageStats)
		return ConsumeModelUsageStats(connectwire.BytesValue(f), m.Usage)
	}
	return nil
}

func consumeChatResponseThinking(f connectwire.Field, m *GetChatMessageResponse) error {
	switch f.Number {
	case 8:
		return connectwire.SetBool(f, "devin chat response", &m.Redact)
	case 9:
		return connectwire.SetBytes(f, "devin chat response", &m.DeltaThinkingView)
	case 10:
		return connectwire.SetBytes(f, "devin chat response", &m.DeltaSignatureView)
	case 11:
		return connectwire.SetBool(f, "devin chat response", &m.ThinkingRedacted)
	case 12:
		return setFloat64(f, "devin chat response", &m.Latency)
	case 13:
		return connectwire.SetBytes(f, "devin chat response", &m.CompletionProfileView)
	}
	return nil
}

func consumeChatResponseBilling(f connectwire.Field, m *GetChatMessageResponse) error {
	switch f.Number {
	case 14:
		return connectwire.SetInt32(f, "devin chat response", &m.CreditCost)
	case 15:
		return connectwire.SetBytes(f, "devin chat response", &m.OutputIDView)
	case 16:
		return connectwire.SetBytes(f, "devin chat response", &m.ThinkingIDView)
	case 17:
		return connectwire.SetBytes(f, "devin chat response", &m.RequestIDView)
	case 18:
		return connectwire.SetInt32(f, "devin chat response", &m.CommittedCreditCost)
	case 19:
		return connectwire.SetBytes(f, "devin chat response", &m.PromptView)
	case 20:
		return connectwire.SetBytes(f, "devin chat response", &m.GeminiSignatureView)
	case 21:
		return connectwire.SetBytes(f, "devin chat response", &m.SignatureTypeView)
	}
	return nil
}

func consumeChatResponseRouting(f connectwire.Field, m *GetChatMessageResponse) error {
	switch f.Number {
	case 22:
		return setFloat64(f, "devin chat response", &m.CommittedACUCost)
	case 23:
		return connectwire.SetBytes(f, "devin chat response", &m.ActualModelUIDView)
	case 24:
		return connectwire.SetBool(f, "devin chat response", &m.ArenaInvocationCapReached)
	case 25:
		return connectwire.SetBytes(f, "devin chat response", &m.PhaseView)
	case 26:
		return connectwire.SetInt64(f, "devin chat response", &m.CommittedQuotaCost)
	case 27:
		return connectwire.SetInt64(f, "devin chat response", &m.CommittedOverageCost)
	case 28:
		if err := connectwire.RequireKind(f, protowire.BytesType, "devin chat response"); err != nil {
			return err
		}
		m.ResponseDimensionGroups = append(m.ResponseDimensionGroups, connectwire.BytesValue(f))
	}
	return nil
}
