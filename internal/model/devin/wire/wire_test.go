package wire_test

import (
	"bytes"
	"math"
	"math/rand/v2"
	"reflect"
	"testing"

	"github.com/gosuda/dalgona/internal/model/devin/wire"
	"google.golang.org/protobuf/encoding/protowire"
)

func randomBytes(r *rand.Rand) []byte {
	out := make([]byte, 1+r.IntN(9))
	for i := range out {
		out[i] = byte(r.Uint32() & 0xff)
	}
	return out
}

func randomList(r *rand.Rand) [][]byte {
	out := make([][]byte, 1+r.IntN(3))
	for i := range out {
		out[i] = randomBytes(r)
	}
	return out
}

func checkRoundTrip[T any](
	t *testing.T,
	name string,
	makeValue func(*rand.Rand) T,
	appendValue func([]byte, *T) []byte,
	consumeValue func([]byte, *T) error,
) {
	t.Helper()
	t.Run(name, func(t *testing.T) {
		t.Parallel()
		r := rand.New(rand.NewPCG(0x5eed, 0xc0dec))
		for range 64 {
			want := makeValue(r)
			framed := appendValue([]byte{0xa5}, &want)
			var got T
			if err := consumeValue(framed[1:], &got); err != nil {
				t.Fatalf("decode: %v", err)
			}
			if !reflect.DeepEqual(got, want) {
				t.Fatalf("round trip mismatch\n got: %#v\nwant: %#v", got, want)
			}
		}
	})
}

func TestMessageRoundTrips(t *testing.T) {
	t.Parallel()
	checkRoundTrip(t, "tool call", func(r *rand.Rand) wire.ChatToolCall {
		return wire.ChatToolCall{IDView: randomBytes(r), NameView: randomBytes(r), ArgumentsView: randomBytes(r), InvalidJSONView: randomBytes(r), InvalidErrorView: randomBytes(r), Custom: true}
	}, wire.AppendChatToolCall, wire.ConsumeChatToolCall)
	checkRoundTrip(t, "chat prompt", func(r *rand.Rand) wire.ChatMessagePrompt {
		return wire.ChatMessagePrompt{MessageIDView: randomBytes(r), Source: -2, PromptView: randomBytes(r), NumTokens: 123, SafeForCodeTelemetry: true, ToolCalls: []wire.ChatToolCall{{IDView: randomBytes(r), NameView: randomBytes(r)}}, ToolCallIDView: randomBytes(r), PromptCacheOptions: &wire.PromptCacheOptions{Type: 1}, ToolResultIsError: true, Images: []wire.ImageData{{Base64DataView: randomBytes(r), MimeTypeView: randomBytes(r)}}, ThinkingView: randomBytes(r), SignatureView: randomBytes(r), ThinkingRedacted: true, Annotations: randomList(r), OutputIDView: randomBytes(r), ThinkingIDView: randomBytes(r), GeminiSignatureView: randomBytes(r), SignatureTypeView: randomBytes(r), PhaseView: randomBytes(r)}
	}, wire.AppendChatMessagePrompt, wire.ConsumeChatMessagePrompt)
	checkRoundTrip(t, "tool definition", func(r *rand.Rand) wire.ChatToolDefinition {
		return wire.ChatToolDefinition{NameView: randomBytes(r), DescriptionView: randomBytes(r), JSONSchemaView: randomBytes(r), AttributionFieldNames: randomList(r), ServerNameView: randomBytes(r), ReadOnlyHint: true, ComputerUseConfigView: randomBytes(r), CustomTool: true, CustomToolGrammarView: randomBytes(r), CustomGrammarSyntaxView: randomBytes(r), Strict: true}
	}, wire.AppendChatToolDefinition, wire.ConsumeChatToolDefinition)
	checkRoundTrip(t, "tool choice", func(r *rand.Rand) wire.ChatToolChoice {
		return wire.ChatToolChoice{ChoiceNumber: 2, ChoiceView: randomBytes(r)}
	}, wire.AppendChatToolChoice, wire.ConsumeChatToolChoice)
	checkRoundTrip(t, "chat request", func(r *rand.Rand) wire.GetChatMessageRequest {
		return wire.GetChatMessageRequest{Metadata: &wire.Metadata{APIKeyView: randomBytes(r), UserJWTView: randomBytes(r)}, PromptView: randomBytes(r), Prompts: []wire.ChatMessagePrompt{{MessageIDView: randomBytes(r), Source: 2, PromptView: randomBytes(r)}}, UseInternalChatModel: true, InternalChatModel: -4, RequestType: 5, Configuration: &wire.CompletionConfiguration{MaxTokens: 64000, TopP: 0.95}, ExperimentConfigView: randomBytes(r), Tools: []wire.ChatToolDefinition{{NameView: randomBytes(r), Strict: true}}, DisableToolCalls: true, ToolChoice: &wire.ChatToolChoice{ChoiceNumber: 1, ChoiceView: randomBytes(r)}, SystemPromptCache: &wire.PromptCacheOptions{Type: 1}, ChatModelNameView: randomBytes(r), TrajectoryReferenceView: randomBytes(r), ConversationIDView: randomBytes(r), PromptIDView: randomBytes(r), ProviderSource: -3, Language: 7, PlannerMode: 1, ChatModelUIDView: randomBytes(r), ExecutionIDView: randomBytes(r), ArenaConvergeCount: new(r.Int32() - r.Int32()), ArenaAssignmentJWTView: randomBytes(r), ModelAssignmentJWTView: randomBytes(r)}
	}, wire.AppendGetChatMessageRequest, wire.ConsumeGetChatMessageRequest)
	checkRoundTrip(t, "timestamp", func(r *rand.Rand) wire.Timestamp {
		return wire.Timestamp{Seconds: r.Int64() - r.Int64(), Nanos: r.Int32() - r.Int32()}
	}, wire.AppendTimestamp, wire.ConsumeTimestamp)
	checkRoundTrip(t, "metadata", func(r *rand.Rand) wire.Metadata {
		return wire.Metadata{IdeNameView: randomBytes(r), IdeVersionView: randomBytes(r), IdeTypeView: randomBytes(r), ExtensionNameView: randomBytes(r), ExtensionVersionView: randomBytes(r), APIKeyView: randomBytes(r), LocaleView: randomBytes(r), OSView: randomBytes(r), HardwareView: randomBytes(r), DisableTelemetry: true, SessionIDView: randomBytes(r), LSTimestamp: &wire.Timestamp{Seconds: r.Int64()}, RequestID: r.Uint64(), SourceAddressView: randomBytes(r), UserAgentView: randomBytes(r), URLView: randomBytes(r), AuthSource: r.Int32(), ExtensionPathView: randomBytes(r), UserIDView: randomBytes(r), UserJWTView: randomBytes(r), ForceTeamIDView: randomBytes(r), DeviceFingerprintView: randomBytes(r), TriggerIDView: randomBytes(r), PlanNameView: randomBytes(r), IDView: randomBytes(r), ImpersonateTierView: randomBytes(r), SupportedModelDisplays: []int32{0, 3, -1, r.Int32() - r.Int32(), 8}, FView: randomBytes(r), TeamIDView: randomBytes(r)}
	}, wire.AppendMetadata, wire.ConsumeMetadata)
	checkRoundTrip(t, "completion configuration", func(r *rand.Rand) wire.CompletionConfiguration {
		return wire.CompletionConfiguration{NumCompletions: r.Uint64(), MaxTokens: r.Uint64(), MaxNewlines: r.Uint64(), MinLogProbability: r.NormFloat64(), Temperature: r.Float64(), FirstTemperature: r.Float64(), TopK: r.Uint64(), TopP: r.Float64(), StopPatterns: randomList(r), Seed: r.Uint64(), FimEOTProbThreshold: r.Float64(), UseFimEOTThreshold: true, DoNotScoreStopTokens: true, SqrtLenNormalizedLogProbScore: true, LastMessageIsPartial: true, ReturnLogprob: true, ServiceTierView: randomBytes(r)}
	}, wire.AppendCompletionConfiguration, wire.ConsumeCompletionConfiguration)
	checkRoundTrip(t, "image data", func(r *rand.Rand) wire.ImageData {
		return wire.ImageData{Base64DataView: randomBytes(r), MimeTypeView: randomBytes(r), CaptionView: randomBytes(r)}
	}, wire.AppendImageData, wire.ConsumeImageData)
	checkRoundTrip(t, "prompt cache options", func(r *rand.Rand) wire.PromptCacheOptions {
		return wire.PromptCacheOptions{Type: r.Int32()}
	}, wire.AppendPromptCacheOptions, wire.ConsumePromptCacheOptions)
	checkRoundTrip(t, "response header", func(r *rand.Rand) wire.ResponseHeader {
		return wire.ResponseHeader{NameView: randomBytes(r), ValueView: randomBytes(r)}
	}, wire.AppendResponseHeader, wire.ConsumeResponseHeader)
	checkRoundTrip(t, "usage", func(r *rand.Rand) wire.ModelUsageStats {
		return wire.ModelUsageStats{ModelDeprecated: -2, Input: 1 << 60, Output: 23, CacheWrite: 45, CacheRead: 67, APIProvider: 9, MessageIDView: randomBytes(r), ResponseHeaders: []wire.ResponseHeader{{NameView: randomBytes(r), ValueView: randomBytes(r)}}, ModelUIDView: randomBytes(r), BillingUIDView: randomBytes(r), RequestedUIDView: randomBytes(r)}
	}, wire.AppendModelUsageStats, wire.ConsumeModelUsageStats)
	checkRoundTrip(t, "chat response", func(r *rand.Rand) wire.GetChatMessageResponse {
		return wire.GetChatMessageResponse{MessageIDView: randomBytes(r), Timestamp: &wire.Timestamp{Seconds: -5, Nanos: 7}, DeltaTextView: randomBytes(r), DeltaTokens: 77, StopReason: -10, ToolCalls: []wire.ChatToolCall{{IDView: randomBytes(r), ArgumentsView: randomBytes(r)}}, Usage: &wire.ModelUsageStats{Input: 10, CacheRead: 4}, Redact: true, DeltaThinkingView: randomBytes(r), DeltaSignatureView: randomBytes(r), ThinkingRedacted: true, Latency: 1.25, CompletionProfileView: randomBytes(r), CreditCost: -3, OutputIDView: randomBytes(r), ThinkingIDView: randomBytes(r), RequestIDView: randomBytes(r), CommittedCreditCost: -9, PromptView: randomBytes(r), GeminiSignatureView: randomBytes(r), SignatureTypeView: randomBytes(r), CommittedACUCost: 2.5, ActualModelUIDView: randomBytes(r), ArenaInvocationCapReached: true, PhaseView: randomBytes(r), CommittedQuotaCost: -1 << 50, CommittedOverageCost: -99, ResponseDimensionGroups: randomList(r)}
	}, wire.AppendGetChatMessageResponse, wire.ConsumeGetChatMessageResponse)
	checkRoundTrip(t, "model info", func(r *rand.Rand) wire.ModelInfo {
		return wire.ModelInfo{ModelID: -1, ModelUIDView: randomBytes(r), Internal: true, ModelType: 2, MaxTokens: 100000, TokenizerTypeView: randomBytes(r), ModelFeaturesView: randomBytes(r), APIProvider: 4, ModelNameView: randomBytes(r), SupportsContext: true, EmbedDim: -4, BaseURLView: randomBytes(r), ChatModelNameView: randomBytes(r), MaxOutputTokens: 8192, PromptTemplaterType: 3, ToolFormatterType: 5, InferenceServerURLView: randomBytes(r), HarnessUIDs: randomList(r), ArenaConfigView: randomBytes(r), DisplayOption: 8, ModelFamilyUIDView: randomBytes(r), InferenceConfigView: randomBytes(r), ModelRouter: true}
	}, wire.AppendModelInfo, wire.ConsumeModelInfo)
	checkRoundTrip(t, "model config", func(r *rand.Rand) wire.ClientModelConfig {
		return wire.ClientModelConfig{LabelView: randomBytes(r), ModelOrAliasView: randomBytes(r), ModelUIDView: randomBytes(r), CreditMultiplier: 1.75, PricingType: 4, Disabled: true, SupportsImages: true, SupportsLegacy: true, Premium: true, BetaWarningMessageView: randomBytes(r), Beta: true, Provider: 8, Recommended: true, AllowedTiers: []int32{0, -1, 2, r.Int32() - r.Int32()}, APIProvider: 6, New: true, PartialRollout: true, RolloutFraction: 0.75, ContextWindow: 200000, PromoStatusView: randomBytes(r), CapacityLimited: true, FastStatusView: randomBytes(r), ModelInfo: &wire.ModelInfo{MaxOutputTokens: 4096, ModelUIDView: randomBytes(r)}, ModelCostTier: 2, DescriptionView: randomBytes(r), SmartFriendModelUIDView: randomBytes(r), ModelFamilyMetadataView: randomBytes(r), DefaultModelInFamily: true, ModelDimensions: randomList(r), DisabledReasonView: randomBytes(r)}
	}, wire.AppendClientModelConfig, wire.ConsumeClientModelConfig)
	checkRoundTrip(t, "model config request", func(r *rand.Rand) wire.ModelConfigRequest {
		return wire.ModelConfigRequest{Metadata: &wire.Metadata{IdeNameView: randomBytes(r), SupportedModelDisplays: []int32{3, 4, 6}}}
	}, wire.AppendModelConfigRequest, wire.ConsumeModelConfigRequest)
	checkRoundTrip(t, "model config response", func(r *rand.Rand) wire.ModelConfigResponse {
		return wire.ModelConfigResponse{Models: []wire.ClientModelConfig{{LabelView: randomBytes(r), ModelUIDView: randomBytes(r), CreditMultiplier: 2.5}}, DefaultOverrideConfigView: randomBytes(r)}
	}, wire.AppendModelConfigResponse, wire.ConsumeModelConfigResponse)
	checkRoundTrip(t, "JWT request", func(r *rand.Rand) wire.GetUserJWTRequest {
		return wire.GetUserJWTRequest{Metadata: &wire.Metadata{APIKeyView: randomBytes(r)}}
	}, wire.AppendGetUserJWTRequest, wire.ConsumeGetUserJWTRequest)
	checkRoundTrip(t, "JWT response", func(r *rand.Rand) wire.GetUserJWTResponse {
		return wire.GetUserJWTResponse{UserJWTView: randomBytes(r), CustomAPIServerURLView: randomBytes(r)}
	}, wire.AppendGetUserJWTResponse, wire.ConsumeGetUserJWTResponse)
}

func TestHandAssembledChatRequest(t *testing.T) {
	t.Parallel()
	prompt := protowire.AppendTag(nil, 1, protowire.BytesType)
	prompt = protowire.AppendString(prompt, "message-a")
	prompt = protowire.AppendTag(prompt, 2, protowire.VarintType)
	prompt = protowire.AppendVarint(prompt, 2)
	prompt = protowire.AppendTag(prompt, 3, protowire.BytesType)
	prompt = protowire.AppendString(prompt, "hello")
	tool := protowire.AppendTag(nil, 1, protowire.BytesType)
	tool = protowire.AppendString(tool, "exec")
	tool = protowire.AppendTag(tool, 12, protowire.VarintType)
	tool = protowire.AppendVarint(tool, 1)
	frame := appendField(3, prompt)
	frame = append(frame, appendField(10, tool)...)
	frame = append(frame, appendField(13, []byte{0x08, 1})...)
	frame = append(frame, appendField(16, []byte("cascade-a"))...)
	frame = append(frame, appendField(21, []byte("model-a"))...)
	frame = protowire.AppendTag(frame, 20, protowire.VarintType)
	frame = protowire.AppendVarint(frame, 1)
	var got wire.GetChatMessageRequest
	if err := wire.ConsumeGetChatMessageRequest(frame, &got); err != nil {
		t.Fatal(err)
	}
	assertChatRequestPrompt(t, &got)
	assertChatRequestRouting(t, &got)
	assertChatRequestTools(t, &got)
}

func assertChatRequestPrompt(t *testing.T, got *wire.GetChatMessageRequest) {
	t.Helper()
	if len(got.Prompts) != 1 || got.Prompts[0].Source != 2 || string(got.Prompts[0].PromptView) != "hello" {
		t.Fatalf("prompts = %#v", got.Prompts)
	}
}

func assertChatRequestRouting(t *testing.T, got *wire.GetChatMessageRequest) {
	t.Helper()
	if string(got.ConversationIDView) != "cascade-a" || string(got.ChatModelUIDView) != "model-a" || got.PlannerMode != 1 {
		t.Fatalf("routing = %#v", got)
	}
}

func assertChatRequestTools(t *testing.T, got *wire.GetChatMessageRequest) {
	t.Helper()
	if len(got.Tools) != 1 || !got.Tools[0].Strict || got.SystemPromptCache == nil || got.SystemPromptCache.Type != 1 {
		t.Fatalf("tools = %#v cache = %#v", got.Tools, got.SystemPromptCache)
	}
}

func TestHandAssembledChatResponse(t *testing.T) {
	t.Parallel()
	usage := protowire.AppendTag(nil, 2, protowire.VarintType)
	usage = protowire.AppendVarint(usage, 100)
	usage = protowire.AppendTag(usage, 5, protowire.VarintType)
	usage = protowire.AppendVarint(usage, 40)
	call := protowire.AppendTag(nil, 1, protowire.BytesType)
	call = protowire.AppendString(call, "call-a")
	call = protowire.AppendTag(call, 3, protowire.BytesType)
	call = protowire.AppendString(call, `{}`)
	frame := appendField(6, call)
	frame = append(frame, appendField(7, usage)...)
	frame = protowire.AppendTag(frame, 4, protowire.VarintType)
	frame = protowire.AppendVarint(frame, 8)
	frame = protowire.AppendTag(frame, 5, protowire.VarintType)
	frame = protowire.AppendVarint(frame, ^uint64(0))
	frame = protowire.AppendTag(frame, 12, protowire.Fixed64Type)
	frame = protowire.AppendFixed64(frame, math.Float64bits(3.25))
	var got wire.GetChatMessageResponse
	if err := wire.ConsumeGetChatMessageResponse(frame, &got); err != nil {
		t.Fatal(err)
	}
	assertChatResponseScalars(t, &got)
	assertChatResponseUsage(t, &got)
}

func assertChatResponseScalars(t *testing.T, got *wire.GetChatMessageResponse) {
	t.Helper()
	if got.DeltaTokens != 8 || got.StopReason != -1 || got.Latency != 3.25 {
		t.Fatalf("response = %#v", got)
	}
}

func assertChatResponseUsage(t *testing.T, got *wire.GetChatMessageResponse) {
	t.Helper()
	if got.Usage == nil || got.Usage.Input != 100 || got.Usage.CacheRead != 40 || len(got.ToolCalls) != 1 || string(got.ToolCalls[0].IDView) != "call-a" {
		t.Fatalf("usage = %#v calls = %#v", got.Usage, got.ToolCalls)
	}
}

func TestHandAssembledModelConfig(t *testing.T) {
	t.Parallel()
	info := protowire.AppendTag(nil, 13, protowire.VarintType)
	info = protowire.AppendVarint(info, 8192)
	model := protowire.AppendTag(nil, 1, protowire.BytesType)
	model = protowire.AppendString(model, "Model A")
	model = protowire.AppendTag(model, 3, protowire.Fixed32Type)
	model = protowire.AppendFixed32(model, math.Float32bits(1.5))
	model = append(model, appendField(22, []byte("model-a"))...)
	model = append(model, appendField(23, info)...)
	model = append(model, appendField(27, []byte("description"))...)
	packed := protowire.AppendVarint(nil, 2)
	packed = protowire.AppendVarint(packed, 4)
	model = append(model, appendField(12, packed)...)
	response := appendField(1, model)
	var got wire.ModelConfigResponse
	if err := wire.ConsumeModelConfigResponse(response, &got); err != nil {
		t.Fatal(err)
	}
	if len(got.Models) != 1 {
		t.Fatalf("models = %#v", got.Models)
	}
	config := got.Models[0]
	if config.CreditMultiplier != 1.5 || string(config.ModelUIDView) != "model-a" || string(config.DescriptionView) != "description" {
		t.Fatalf("config = %#v", config)
	}
	if config.ModelInfo == nil || config.ModelInfo.MaxOutputTokens != 8192 || !reflect.DeepEqual(config.AllowedTiers, []int32{2, 4}) {
		t.Fatalf("info = %#v tiers = %v", config.ModelInfo, config.AllowedTiers)
	}
}

func TestHandAssembledMetadata(t *testing.T) {
	t.Parallel()
	frame := appendField(1, []byte("devin-cli"))
	frame = append(frame, appendField(3, []byte("key"))...)
	frame = append(frame, appendField(21, []byte("jwt"))...)
	frame = append(frame, appendField(28, []byte("chisel"))...)
	frame = protowire.AppendTag(frame, 9, protowire.VarintType)
	frame = protowire.AppendVarint(frame, 1<<40)
	packed := protowire.AppendVarint(nil, 3)
	packed = protowire.AppendVarint(packed, 8)
	frame = append(frame, appendField(30, packed)...)
	frame = protowire.AppendTag(frame, 30, protowire.VarintType)
	frame = protowire.AppendVarint(frame, 4)
	request := appendField(1, frame)
	var got wire.GetUserJWTRequest
	if err := wire.ConsumeGetUserJWTRequest(request, &got); err != nil {
		t.Fatal(err)
	}
	m := got.Metadata
	if m == nil || string(m.IdeNameView) != "devin-cli" || string(m.APIKeyView) != "key" || string(m.UserJWTView) != "jwt" || string(m.IdeTypeView) != "chisel" || m.RequestID != 1<<40 {
		t.Fatalf("metadata = %#v", m)
	}
	if !reflect.DeepEqual(m.SupportedModelDisplays, []int32{3, 8, 4}) {
		t.Fatalf("displays = %v", m.SupportedModelDisplays)
	}
}

func TestPresenceAndPackedZeros(t *testing.T) {
	t.Parallel()
	zero := int32(0)
	request := wire.AppendGetChatMessageRequest(nil, &wire.GetChatMessageRequest{ArenaConvergeCount: &zero})
	want := protowire.AppendTag(nil, 24, protowire.VarintType)
	want = protowire.AppendVarint(want, 0)
	if !bytes.Equal(request, want) {
		t.Fatalf("explicit zero = %x, want %x", request, want)
	}
	if got := wire.AppendGetChatMessageRequest(nil, &wire.GetChatMessageRequest{}); len(got) != 0 {
		t.Fatalf("absent optional encoded as %x", got)
	}
	metadata := wire.AppendMetadata(nil, &wire.Metadata{SupportedModelDisplays: []int32{0, 0}})
	packed := protowire.AppendTag(nil, 30, protowire.BytesType)
	packed = protowire.AppendBytes(packed, []byte{0, 0})
	if !bytes.Equal(metadata, packed) {
		t.Fatalf("packed zeros = %x, want %x", metadata, packed)
	}
}

func TestEmptyToolChoiceVariant(t *testing.T) {
	t.Parallel()
	got := wire.AppendChatToolChoice(nil, &wire.ChatToolChoice{ChoiceNumber: 2})
	want := protowire.AppendTag(nil, 2, protowire.BytesType)
	want = protowire.AppendBytes(want, nil)
	if !bytes.Equal(got, want) {
		t.Fatalf("frame = %x, want %x", got, want)
	}
}

func TestUnknownFieldsAndTruncation(t *testing.T) {
	t.Parallel()
	for _, tc := range decoderTable() {
		t.Run(tc.name+" unknown fields", func(t *testing.T) {
			t.Parallel()
			if err := tc.decode(unknownFields()); err != nil {
				t.Fatalf("unknown fields: %v", err)
			}
		})
		t.Run(tc.name+" truncated", func(t *testing.T) {
			t.Parallel()
			if err := tc.decode([]byte{0x0a, 5, 'x'}); err == nil {
				t.Fatal("truncated bytes field was accepted")
			}
		})
	}
}

func TestDecodedViewsAliasInput(t *testing.T) {
	t.Parallel()
	input := appendField(3, []byte("abc"))
	var got wire.GetChatMessageResponse
	if err := wire.ConsumeGetChatMessageResponse(input, &got); err != nil {
		t.Fatal(err)
	}
	input[len(input)-1] = 'z'
	if string(got.DeltaTextView) != "abz" {
		t.Fatalf("view copied input: %q", got.DeltaTextView)
	}
}

type decoderCase struct {
	name   string
	decode func([]byte) error
}

func decoderTable() []decoderCase {
	return []decoderCase{
		{"tool call", func(b []byte) error { return wire.ConsumeChatToolCall(b, new(wire.ChatToolCall)) }},
		{"chat prompt", func(b []byte) error { return wire.ConsumeChatMessagePrompt(b, new(wire.ChatMessagePrompt)) }},
		{"tool definition", func(b []byte) error { return wire.ConsumeChatToolDefinition(b, new(wire.ChatToolDefinition)) }},
		{"tool choice", func(b []byte) error { return wire.ConsumeChatToolChoice(b, new(wire.ChatToolChoice)) }},
		{"chat request", func(b []byte) error { return wire.ConsumeGetChatMessageRequest(b, new(wire.GetChatMessageRequest)) }},
		{"response header", func(b []byte) error { return wire.ConsumeResponseHeader(b, new(wire.ResponseHeader)) }},
		{"usage", func(b []byte) error { return wire.ConsumeModelUsageStats(b, new(wire.ModelUsageStats)) }},
		{"chat response", func(b []byte) error { return wire.ConsumeGetChatMessageResponse(b, new(wire.GetChatMessageResponse)) }},
		{"model info", func(b []byte) error { return wire.ConsumeModelInfo(b, new(wire.ModelInfo)) }},
		{"model config", func(b []byte) error { return wire.ConsumeClientModelConfig(b, new(wire.ClientModelConfig)) }},
		{"model config request", func(b []byte) error { return wire.ConsumeModelConfigRequest(b, new(wire.ModelConfigRequest)) }},
		{"model config response", func(b []byte) error { return wire.ConsumeModelConfigResponse(b, new(wire.ModelConfigResponse)) }},
		{"timestamp", func(b []byte) error { return wire.ConsumeTimestamp(b, new(wire.Timestamp)) }},
		{"metadata", func(b []byte) error { return wire.ConsumeMetadata(b, new(wire.Metadata)) }},
		{"completion configuration", func(b []byte) error { return wire.ConsumeCompletionConfiguration(b, new(wire.CompletionConfiguration)) }},
		{"image data", func(b []byte) error { return wire.ConsumeImageData(b, new(wire.ImageData)) }},
		{"prompt cache options", func(b []byte) error { return wire.ConsumePromptCacheOptions(b, new(wire.PromptCacheOptions)) }},
		{"JWT request", func(b []byte) error { return wire.ConsumeGetUserJWTRequest(b, new(wire.GetUserJWTRequest)) }},
		{"JWT response", func(b []byte) error { return wire.ConsumeGetUserJWTResponse(b, new(wire.GetUserJWTResponse)) }},
	}
}

func unknownFields() []byte {
	out := protowire.AppendTag(nil, 1001, protowire.VarintType)
	out = protowire.AppendVarint(out, 123456)
	out = protowire.AppendTag(out, 1002, protowire.Fixed32Type)
	out = protowire.AppendFixed32(out, 0xdeadbeef)
	out = protowire.AppendTag(out, 1003, protowire.Fixed64Type)
	out = protowire.AppendFixed64(out, 0x1234567890)
	out = append(out, appendField(1004, []byte("unknown"))...)
	out = protowire.AppendTag(out, 1005, protowire.StartGroupType)
	out = protowire.AppendTag(out, 1, protowire.VarintType)
	out = protowire.AppendVarint(out, 7)
	out = protowire.AppendTag(out, 1005, protowire.EndGroupType)
	return out
}

func appendField(number protowire.Number, value []byte) []byte {
	out := protowire.AppendTag(nil, number, protowire.BytesType)
	return protowire.AppendBytes(out, value)
}
