package wire

import (
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"google.golang.org/protobuf/encoding/protowire"
)

type Timestamp struct {
	Seconds int64
	Nanos   int32
}

func AppendTimestamp(dst []byte, m *Timestamp) []byte {
	dst = connectwire.AppendInt64(dst, 1, m.Seconds)
	return connectwire.AppendInt32(dst, 2, m.Nanos)
}

func ConsumeTimestamp(b []byte, m *Timestamp) error {
	return connectwire.ConsumeFields(b, "devin timestamp", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetInt64(f, "devin timestamp", &m.Seconds)
		case 2:
			return connectwire.SetInt32(f, "devin timestamp", &m.Nanos)
		default:
			return nil
		}
	})
}

type Metadata struct {
	IdeNameView            []byte
	IdeVersionView         []byte
	IdeTypeView            []byte
	ExtensionNameView      []byte
	ExtensionVersionView   []byte
	APIKeyView             []byte
	LocaleView             []byte
	OSView                 []byte
	HardwareView           []byte
	DisableTelemetry       bool
	SessionIDView          []byte
	LSTimestamp            *Timestamp
	RequestID              uint64
	SourceAddressView      []byte
	UserAgentView          []byte
	URLView                []byte
	AuthSource             int32
	ExtensionPathView      []byte
	UserIDView             []byte
	UserJWTView            []byte
	ForceTeamIDView        []byte
	DeviceFingerprintView  []byte
	TriggerIDView          []byte
	PlanNameView           []byte
	IDView                 []byte
	ImpersonateTierView    []byte
	SupportedModelDisplays []int32
	FView                  []byte
	TeamIDView             []byte
}

func AppendMetadata(dst []byte, m *Metadata) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.IdeNameView)
	dst = connectwire.AppendRaw(dst, 2, m.ExtensionVersionView)
	dst = connectwire.AppendRaw(dst, 3, m.APIKeyView)
	dst = connectwire.AppendRaw(dst, 4, m.LocaleView)
	dst = connectwire.AppendRaw(dst, 5, m.OSView)
	dst = connectwire.AppendBool(dst, 6, m.DisableTelemetry)
	dst = connectwire.AppendRaw(dst, 7, m.IdeVersionView)
	dst = connectwire.AppendRaw(dst, 8, m.HardwareView)
	dst = connectwire.AppendUint64(dst, 9, m.RequestID)
	dst = connectwire.AppendRaw(dst, 10, m.SessionIDView)
	dst = connectwire.AppendRaw(dst, 11, m.SourceAddressView)
	dst = connectwire.AppendRaw(dst, 12, m.ExtensionNameView)
	dst = connectwire.AppendRaw(dst, 13, m.UserAgentView)
	dst = connectwire.AppendRaw(dst, 14, m.URLView)
	dst = connectwire.AppendInt32(dst, 15, m.AuthSource)
	if m.LSTimestamp != nil {
		dst = connectwire.AppendMessage(dst, 16, m.LSTimestamp, AppendTimestamp)
	}
	dst = connectwire.AppendRaw(dst, 17, m.ExtensionPathView)
	dst = connectwire.AppendRaw(dst, 20, m.UserIDView)
	dst = connectwire.AppendRaw(dst, 21, m.UserJWTView)
	dst = connectwire.AppendRaw(dst, 22, m.ForceTeamIDView)
	dst = connectwire.AppendRaw(dst, 24, m.DeviceFingerprintView)
	dst = connectwire.AppendRaw(dst, 25, m.TriggerIDView)
	dst = connectwire.AppendRaw(dst, 26, m.PlanNameView)
	dst = connectwire.AppendRaw(dst, 27, m.IDView)
	dst = connectwire.AppendRaw(dst, 28, m.IdeTypeView)
	dst = connectwire.AppendRaw(dst, 29, m.ImpersonateTierView)
	dst = appendPackedInt32(dst, 30, m.SupportedModelDisplays)
	dst = connectwire.AppendRaw(dst, 31, m.FView)
	return connectwire.AppendRaw(dst, 32, m.TeamIDView)
}

func ConsumeMetadata(b []byte, m *Metadata) error {
	return connectwire.ConsumeFields(b, "devin metadata", func(f connectwire.Field) error {
		return consumeMetadataField(f, m)
	})
}

func consumeMetadataField(f connectwire.Field, m *Metadata) error {
	switch {
	case f.Number <= 6:
		return consumeMetadataClient(f, m)
	case f.Number <= 12:
		return consumeMetadataEnvironment(f, m)
	case f.Number <= 17:
		return consumeMetadataNetwork(f, m)
	case f.Number <= 26:
		return consumeMetadataUser(f, m)
	default:
		return consumeMetadataAccount(f, m)
	}
}

func consumeMetadataClient(f connectwire.Field, m *Metadata) error {
	switch f.Number {
	case 1:
		return connectwire.SetBytes(f, "devin metadata", &m.IdeNameView)
	case 2:
		return connectwire.SetBytes(f, "devin metadata", &m.ExtensionVersionView)
	case 3:
		return connectwire.SetBytes(f, "devin metadata", &m.APIKeyView)
	case 4:
		return connectwire.SetBytes(f, "devin metadata", &m.LocaleView)
	case 5:
		return connectwire.SetBytes(f, "devin metadata", &m.OSView)
	case 6:
		return connectwire.SetBool(f, "devin metadata", &m.DisableTelemetry)
	default:
		return nil
	}
}

func consumeMetadataEnvironment(f connectwire.Field, m *Metadata) error {
	switch f.Number {
	case 7:
		return connectwire.SetBytes(f, "devin metadata", &m.IdeVersionView)
	case 8:
		return connectwire.SetBytes(f, "devin metadata", &m.HardwareView)
	case 9:
		return connectwire.SetUint64(f, "devin metadata", &m.RequestID)
	case 10:
		return connectwire.SetBytes(f, "devin metadata", &m.SessionIDView)
	case 11:
		return connectwire.SetBytes(f, "devin metadata", &m.SourceAddressView)
	case 12:
		return connectwire.SetBytes(f, "devin metadata", &m.ExtensionNameView)
	default:
		return nil
	}
}

func consumeMetadataNetwork(f connectwire.Field, m *Metadata) error {
	switch f.Number {
	case 13:
		return connectwire.SetBytes(f, "devin metadata", &m.UserAgentView)
	case 14:
		return connectwire.SetBytes(f, "devin metadata", &m.URLView)
	case 15:
		return connectwire.SetInt32(f, "devin metadata", &m.AuthSource)
	case 16:
		return connectwire.ConsumeNested(f, "devin metadata", &m.LSTimestamp, ConsumeTimestamp)
	case 17:
		return connectwire.SetBytes(f, "devin metadata", &m.ExtensionPathView)
	default:
		return nil
	}
}

func consumeMetadataUser(f connectwire.Field, m *Metadata) error {
	switch f.Number {
	case 20:
		return connectwire.SetBytes(f, "devin metadata", &m.UserIDView)
	case 21:
		return connectwire.SetBytes(f, "devin metadata", &m.UserJWTView)
	case 22:
		return connectwire.SetBytes(f, "devin metadata", &m.ForceTeamIDView)
	case 24:
		return connectwire.SetBytes(f, "devin metadata", &m.DeviceFingerprintView)
	case 25:
		return connectwire.SetBytes(f, "devin metadata", &m.TriggerIDView)
	case 26:
		return connectwire.SetBytes(f, "devin metadata", &m.PlanNameView)
	default:
		return nil
	}
}

func consumeMetadataAccount(f connectwire.Field, m *Metadata) error {
	switch f.Number {
	case 27:
		return connectwire.SetBytes(f, "devin metadata", &m.IDView)
	case 28:
		return connectwire.SetBytes(f, "devin metadata", &m.IdeTypeView)
	case 29:
		return connectwire.SetBytes(f, "devin metadata", &m.ImpersonateTierView)
	case 30:
		return consumeModelDisplay(f, m)
	case 31:
		return connectwire.SetBytes(f, "devin metadata", &m.FView)
	case 32:
		return connectwire.SetBytes(f, "devin metadata", &m.TeamIDView)
	default:
		return nil
	}
}

func consumeModelDisplay(f connectwire.Field, m *Metadata) error {
	if f.Kind == protowire.VarintType {
		m.SupportedModelDisplays = append(m.SupportedModelDisplays, connectwire.Signed32(f))
		return nil
	}
	if f.Kind != protowire.BytesType {
		return connectwire.RequireKind(f, protowire.VarintType, "devin metadata")
	}
	packed := connectwire.BytesValue(f)
	for len(packed) > 0 {
		_, size := protowire.ConsumeVarint(packed)
		if size < 0 {
			return protowire.ParseError(size)
		}
		m.SupportedModelDisplays = append(m.SupportedModelDisplays, connectwire.Signed32(connectwire.Field{Raw: packed[:size]}))
		packed = packed[size:]
	}
	return nil
}

type CompletionConfiguration struct {
	NumCompletions                uint64
	MaxTokens                     uint64
	MaxNewlines                   uint64
	MinLogProbability             float64
	Temperature                   float64
	FirstTemperature              float64
	TopK                          uint64
	TopP                          float64
	StopPatterns                  [][]byte
	Seed                          uint64
	FimEOTProbThreshold           float64
	UseFimEOTThreshold            bool
	DoNotScoreStopTokens          bool
	SqrtLenNormalizedLogProbScore bool
	LastMessageIsPartial          bool
	ReturnLogprob                 bool
	ServiceTierView               []byte
}

func AppendCompletionConfiguration(dst []byte, m *CompletionConfiguration) []byte {
	dst = connectwire.AppendUint64(dst, 1, m.NumCompletions)
	dst = connectwire.AppendUint64(dst, 2, m.MaxTokens)
	dst = connectwire.AppendUint64(dst, 3, m.MaxNewlines)
	dst = appendFloat64(dst, 4, m.MinLogProbability)
	dst = appendFloat64(dst, 5, m.Temperature)
	dst = appendFloat64(dst, 6, m.FirstTemperature)
	dst = connectwire.AppendUint64(dst, 7, m.TopK)
	dst = appendFloat64(dst, 8, m.TopP)
	dst = connectwire.AppendRepeatedBytes(dst, 9, m.StopPatterns)
	dst = connectwire.AppendUint64(dst, 10, m.Seed)
	dst = appendFloat64(dst, 11, m.FimEOTProbThreshold)
	dst = connectwire.AppendBool(dst, 12, m.UseFimEOTThreshold)
	dst = connectwire.AppendBool(dst, 13, m.DoNotScoreStopTokens)
	dst = connectwire.AppendBool(dst, 14, m.SqrtLenNormalizedLogProbScore)
	dst = connectwire.AppendBool(dst, 15, m.LastMessageIsPartial)
	dst = connectwire.AppendBool(dst, 16, m.ReturnLogprob)
	return connectwire.AppendRaw(dst, 17, m.ServiceTierView)
}

func ConsumeCompletionConfiguration(b []byte, m *CompletionConfiguration) error {
	return connectwire.ConsumeFields(b, "devin completion config", func(f connectwire.Field) error {
		if f.Number <= 6 {
			return consumeCompletionSampling(f, m)
		}
		if f.Number <= 11 {
			return consumeCompletionThresholds(f, m)
		}
		return consumeCompletionScoring(f, m)
	})
}

func consumeCompletionSampling(f connectwire.Field, m *CompletionConfiguration) error {
	switch f.Number {
	case 1:
		return connectwire.SetUint64(f, "devin completion config", &m.NumCompletions)
	case 2:
		return connectwire.SetUint64(f, "devin completion config", &m.MaxTokens)
	case 3:
		return connectwire.SetUint64(f, "devin completion config", &m.MaxNewlines)
	case 4:
		return setFloat64(f, "devin completion config", &m.MinLogProbability)
	case 5:
		return setFloat64(f, "devin completion config", &m.Temperature)
	case 6:
		return setFloat64(f, "devin completion config", &m.FirstTemperature)
	default:
		return nil
	}
}

func consumeCompletionThresholds(f connectwire.Field, m *CompletionConfiguration) error {
	switch f.Number {
	case 7:
		return connectwire.SetUint64(f, "devin completion config", &m.TopK)
	case 8:
		return setFloat64(f, "devin completion config", &m.TopP)
	case 9:
		return connectwire.AppendBytesField(f, "devin completion config", &m.StopPatterns)
	case 10:
		return connectwire.SetUint64(f, "devin completion config", &m.Seed)
	case 11:
		return setFloat64(f, "devin completion config", &m.FimEOTProbThreshold)
	default:
		return nil
	}
}

func consumeCompletionScoring(f connectwire.Field, m *CompletionConfiguration) error {
	switch f.Number {
	case 12:
		return connectwire.SetBool(f, "devin completion config", &m.UseFimEOTThreshold)
	case 13:
		return connectwire.SetBool(f, "devin completion config", &m.DoNotScoreStopTokens)
	case 14:
		return connectwire.SetBool(f, "devin completion config", &m.SqrtLenNormalizedLogProbScore)
	case 15:
		return connectwire.SetBool(f, "devin completion config", &m.LastMessageIsPartial)
	case 16:
		return connectwire.SetBool(f, "devin completion config", &m.ReturnLogprob)
	case 17:
		return connectwire.SetBytes(f, "devin completion config", &m.ServiceTierView)
	default:
		return nil
	}
}

type ImageData struct {
	Base64DataView []byte
	MimeTypeView   []byte
	CaptionView    []byte
}

func AppendImageData(dst []byte, m *ImageData) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.Base64DataView)
	dst = connectwire.AppendRaw(dst, 2, m.MimeTypeView)
	return connectwire.AppendRaw(dst, 3, m.CaptionView)
}

func ConsumeImageData(b []byte, m *ImageData) error {
	return connectwire.ConsumeFields(b, "devin image", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "devin image", &m.Base64DataView)
		case 2:
			return connectwire.SetBytes(f, "devin image", &m.MimeTypeView)
		case 3:
			return connectwire.SetBytes(f, "devin image", &m.CaptionView)
		default:
			return nil
		}
	})
}

type PromptCacheOptions struct {
	Type int32
}

func AppendPromptCacheOptions(dst []byte, m *PromptCacheOptions) []byte {
	return connectwire.AppendInt32(dst, 1, m.Type)
}

func ConsumePromptCacheOptions(b []byte, m *PromptCacheOptions) error {
	return connectwire.ConsumeFields(b, "devin cache options", func(f connectwire.Field) error {
		if f.Number != 1 {
			return nil
		}
		return connectwire.SetInt32(f, "devin cache options", &m.Type)
	})
}
