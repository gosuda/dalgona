package wire

import "google.golang.org/protobuf/encoding/protowire"

type Timestamp struct {
	Seconds int64
	Nanos   int32
}

func AppendTimestamp(dst []byte, m *Timestamp) []byte {
	dst = appendInt64(dst, 1, m.Seconds)
	return appendInt32(dst, 2, m.Nanos)
}

func ConsumeTimestamp(b []byte, m *Timestamp) error {
	return consumeFields(b, "devin timestamp", func(f field) error {
		switch f.number {
		case 1:
			return setInt64(f, "devin timestamp", &m.Seconds)
		case 2:
			return setInt32(f, "devin timestamp", &m.Nanos)
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
	dst = appendRaw(dst, 1, m.IdeNameView)
	dst = appendRaw(dst, 2, m.ExtensionVersionView)
	dst = appendRaw(dst, 3, m.APIKeyView)
	dst = appendRaw(dst, 4, m.LocaleView)
	dst = appendRaw(dst, 5, m.OSView)
	dst = appendBool(dst, 6, m.DisableTelemetry)
	dst = appendRaw(dst, 7, m.IdeVersionView)
	dst = appendRaw(dst, 8, m.HardwareView)
	dst = appendUint64(dst, 9, m.RequestID)
	dst = appendRaw(dst, 10, m.SessionIDView)
	dst = appendRaw(dst, 11, m.SourceAddressView)
	dst = appendRaw(dst, 12, m.ExtensionNameView)
	dst = appendRaw(dst, 13, m.UserAgentView)
	dst = appendRaw(dst, 14, m.URLView)
	dst = appendInt32(dst, 15, m.AuthSource)
	if m.LSTimestamp != nil {
		dst = appendMessage(dst, 16, m.LSTimestamp, AppendTimestamp)
	}
	dst = appendRaw(dst, 17, m.ExtensionPathView)
	dst = appendRaw(dst, 20, m.UserIDView)
	dst = appendRaw(dst, 21, m.UserJWTView)
	dst = appendRaw(dst, 22, m.ForceTeamIDView)
	dst = appendRaw(dst, 24, m.DeviceFingerprintView)
	dst = appendRaw(dst, 25, m.TriggerIDView)
	dst = appendRaw(dst, 26, m.PlanNameView)
	dst = appendRaw(dst, 27, m.IDView)
	dst = appendRaw(dst, 28, m.IdeTypeView)
	dst = appendRaw(dst, 29, m.ImpersonateTierView)
	dst = appendPackedInt32(dst, 30, m.SupportedModelDisplays)
	dst = appendRaw(dst, 31, m.FView)
	return appendRaw(dst, 32, m.TeamIDView)
}

func ConsumeMetadata(b []byte, m *Metadata) error {
	return consumeFields(b, "devin metadata", func(f field) error {
		switch {
		case f.number <= 7:
			return consumeMetadataClient(f, m)
		case f.number <= 14:
			return consumeMetadataRequest(f, m)
		case f.number <= 22:
			return consumeMetadataAuth(f, m)
		case f.number <= 28:
			return consumeMetadataAccount(f, m)
		default:
			return consumeMetadataTenant(f, m)
		}
	})
}

func consumeMetadataClient(f field, m *Metadata) error {
	switch f.number {
	case 1:
		return setBytes(f, "devin metadata", &m.IdeNameView)
	case 2:
		return setBytes(f, "devin metadata", &m.ExtensionVersionView)
	case 3:
		return setBytes(f, "devin metadata", &m.APIKeyView)
	case 4:
		return setBytes(f, "devin metadata", &m.LocaleView)
	case 5:
		return setBytes(f, "devin metadata", &m.OSView)
	case 6:
		return setBool(f, "devin metadata", &m.DisableTelemetry)
	case 7:
		return setBytes(f, "devin metadata", &m.IdeVersionView)
	}
	return nil
}

func consumeMetadataRequest(f field, m *Metadata) error {
	switch f.number {
	case 8:
		return setBytes(f, "devin metadata", &m.HardwareView)
	case 9:
		return setUint64(f, "devin metadata", &m.RequestID)
	case 10:
		return setBytes(f, "devin metadata", &m.SessionIDView)
	case 11:
		return setBytes(f, "devin metadata", &m.SourceAddressView)
	case 12:
		return setBytes(f, "devin metadata", &m.ExtensionNameView)
	case 13:
		return setBytes(f, "devin metadata", &m.UserAgentView)
	case 14:
		return setBytes(f, "devin metadata", &m.URLView)
	}
	return nil
}

func consumeMetadataAuth(f field, m *Metadata) error {
	switch f.number {
	case 15:
		return setInt32(f, "devin metadata", &m.AuthSource)
	case 16:
		return consumeNested(f, "devin metadata", &m.LSTimestamp, ConsumeTimestamp)
	case 17:
		return setBytes(f, "devin metadata", &m.ExtensionPathView)
	case 20:
		return setBytes(f, "devin metadata", &m.UserIDView)
	case 21:
		return setBytes(f, "devin metadata", &m.UserJWTView)
	case 22:
		return setBytes(f, "devin metadata", &m.ForceTeamIDView)
	}
	return nil
}

func consumeMetadataAccount(f field, m *Metadata) error {
	switch f.number {
	case 24:
		return setBytes(f, "devin metadata", &m.DeviceFingerprintView)
	case 25:
		return setBytes(f, "devin metadata", &m.TriggerIDView)
	case 26:
		return setBytes(f, "devin metadata", &m.PlanNameView)
	case 27:
		return setBytes(f, "devin metadata", &m.IDView)
	case 28:
		return setBytes(f, "devin metadata", &m.IdeTypeView)
	}
	return nil
}

func consumeMetadataTenant(f field, m *Metadata) error {
	switch f.number {
	case 29:
		return setBytes(f, "devin metadata", &m.ImpersonateTierView)
	case 30:
		return consumeModelDisplay(f, m)
	case 31:
		return setBytes(f, "devin metadata", &m.FView)
	case 32:
		return setBytes(f, "devin metadata", &m.TeamIDView)
	}
	return nil
}

func consumeModelDisplay(f field, m *Metadata) error {
	if f.kind == protowire.VarintType {
		m.SupportedModelDisplays = append(m.SupportedModelDisplays, signed32(f))
		return nil
	}
	if f.kind != protowire.BytesType {
		return requireKind(f, protowire.VarintType, "devin metadata")
	}
	packed := bytesValue(f)
	for len(packed) > 0 {
		_, size := protowire.ConsumeVarint(packed)
		if size < 0 {
			return protowire.ParseError(size)
		}
		m.SupportedModelDisplays = append(m.SupportedModelDisplays, signed32(field{raw: packed[:size]}))
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
	dst = appendUint64(dst, 1, m.NumCompletions)
	dst = appendUint64(dst, 2, m.MaxTokens)
	dst = appendUint64(dst, 3, m.MaxNewlines)
	dst = appendFloat64(dst, 4, m.MinLogProbability)
	dst = appendFloat64(dst, 5, m.Temperature)
	dst = appendFloat64(dst, 6, m.FirstTemperature)
	dst = appendUint64(dst, 7, m.TopK)
	dst = appendFloat64(dst, 8, m.TopP)
	dst = appendRepeatedBytes(dst, 9, m.StopPatterns)
	dst = appendUint64(dst, 10, m.Seed)
	dst = appendFloat64(dst, 11, m.FimEOTProbThreshold)
	dst = appendBool(dst, 12, m.UseFimEOTThreshold)
	dst = appendBool(dst, 13, m.DoNotScoreStopTokens)
	dst = appendBool(dst, 14, m.SqrtLenNormalizedLogProbScore)
	dst = appendBool(dst, 15, m.LastMessageIsPartial)
	dst = appendBool(dst, 16, m.ReturnLogprob)
	return appendRaw(dst, 17, m.ServiceTierView)
}

func ConsumeCompletionConfiguration(b []byte, m *CompletionConfiguration) error {
	return consumeFields(b, "devin completion config", func(f field) error {
		switch {
		case f.number <= 8:
			return consumeConfigNumbers(f, m)
		case f.number <= 12:
			return consumeConfigPatterns(f, m)
		default:
			return consumeConfigFlags(f, m)
		}
	})
}

func consumeConfigNumbers(f field, m *CompletionConfiguration) error {
	switch f.number {
	case 1:
		return setUint64(f, "devin completion config", &m.NumCompletions)
	case 2:
		return setUint64(f, "devin completion config", &m.MaxTokens)
	case 3:
		return setUint64(f, "devin completion config", &m.MaxNewlines)
	case 4:
		return setFloat64(f, "devin completion config", &m.MinLogProbability)
	case 5:
		return setFloat64(f, "devin completion config", &m.Temperature)
	case 6:
		return setFloat64(f, "devin completion config", &m.FirstTemperature)
	case 7:
		return setUint64(f, "devin completion config", &m.TopK)
	case 8:
		return setFloat64(f, "devin completion config", &m.TopP)
	}
	return nil
}

func consumeConfigPatterns(f field, m *CompletionConfiguration) error {
	switch f.number {
	case 9:
		if err := requireKind(f, protowire.BytesType, "devin completion config"); err != nil {
			return err
		}
		m.StopPatterns = append(m.StopPatterns, bytesValue(f))
	case 10:
		return setUint64(f, "devin completion config", &m.Seed)
	case 11:
		return setFloat64(f, "devin completion config", &m.FimEOTProbThreshold)
	case 12:
		return setBool(f, "devin completion config", &m.UseFimEOTThreshold)
	}
	return nil
}

func consumeConfigFlags(f field, m *CompletionConfiguration) error {
	switch f.number {
	case 13:
		return setBool(f, "devin completion config", &m.DoNotScoreStopTokens)
	case 14:
		return setBool(f, "devin completion config", &m.SqrtLenNormalizedLogProbScore)
	case 15:
		return setBool(f, "devin completion config", &m.LastMessageIsPartial)
	case 16:
		return setBool(f, "devin completion config", &m.ReturnLogprob)
	case 17:
		return setBytes(f, "devin completion config", &m.ServiceTierView)
	}
	return nil
}

type ImageData struct {
	Base64DataView []byte
	MimeTypeView   []byte
	CaptionView    []byte
}

func AppendImageData(dst []byte, m *ImageData) []byte {
	dst = appendRaw(dst, 1, m.Base64DataView)
	dst = appendRaw(dst, 2, m.MimeTypeView)
	return appendRaw(dst, 3, m.CaptionView)
}

func ConsumeImageData(b []byte, m *ImageData) error {
	return consumeFields(b, "devin image", func(f field) error {
		switch f.number {
		case 1:
			return setBytes(f, "devin image", &m.Base64DataView)
		case 2:
			return setBytes(f, "devin image", &m.MimeTypeView)
		case 3:
			return setBytes(f, "devin image", &m.CaptionView)
		default:
			return nil
		}
	})
}

type PromptCacheOptions struct {
	Type int32
}

func AppendPromptCacheOptions(dst []byte, m *PromptCacheOptions) []byte {
	return appendInt32(dst, 1, m.Type)
}

func ConsumePromptCacheOptions(b []byte, m *PromptCacheOptions) error {
	return consumeFields(b, "devin cache options", func(f field) error {
		if f.number != 1 {
			return nil
		}
		return setInt32(f, "devin cache options", &m.Type)
	})
}
