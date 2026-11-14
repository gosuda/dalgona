package wire

import (
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"google.golang.org/protobuf/encoding/protowire"
)

type ModelInfo struct {
	ModelID                int32
	ModelUIDView           []byte
	Internal               bool
	ModelType              int32
	MaxTokens              int32
	TokenizerTypeView      []byte
	ModelFeaturesView      []byte
	APIProvider            int32
	ModelNameView          []byte
	SupportsContext        bool
	EmbedDim               int32
	BaseURLView            []byte
	ChatModelNameView      []byte
	MaxOutputTokens        int32
	PromptTemplaterType    int32
	ToolFormatterType      int32
	InferenceServerURLView []byte
	HarnessUIDs            [][]byte
	ArenaConfigView        []byte
	DisplayOption          int32
	ModelFamilyUIDView     []byte
	InferenceConfigView    []byte
	ModelRouter            bool
}

func AppendModelInfo(dst []byte, m *ModelInfo) []byte {
	dst = connectwire.AppendInt32(dst, 1, m.ModelID)
	dst = connectwire.AppendBool(dst, 2, m.Internal)
	dst = connectwire.AppendInt32(dst, 3, m.ModelType)
	dst = connectwire.AppendInt32(dst, 4, m.MaxTokens)
	dst = connectwire.AppendRaw(dst, 5, m.TokenizerTypeView)
	dst = connectwire.AppendRaw(dst, 6, m.ModelFeaturesView)
	dst = connectwire.AppendInt32(dst, 7, m.APIProvider)
	dst = connectwire.AppendRaw(dst, 8, m.ModelNameView)
	dst = connectwire.AppendBool(dst, 9, m.SupportsContext)
	dst = connectwire.AppendInt32(dst, 10, m.EmbedDim)
	dst = connectwire.AppendRaw(dst, 11, m.BaseURLView)
	dst = connectwire.AppendRaw(dst, 12, m.ChatModelNameView)
	dst = connectwire.AppendInt32(dst, 13, m.MaxOutputTokens)
	dst = connectwire.AppendInt32(dst, 14, m.PromptTemplaterType)
	dst = connectwire.AppendInt32(dst, 15, m.ToolFormatterType)
	dst = connectwire.AppendRaw(dst, 17, m.ModelUIDView)
	dst = connectwire.AppendRaw(dst, 18, m.InferenceServerURLView)
	dst = connectwire.AppendRepeatedBytes(dst, 20, m.HarnessUIDs)
	dst = connectwire.AppendRaw(dst, 21, m.ArenaConfigView)
	dst = connectwire.AppendInt32(dst, 22, m.DisplayOption)
	dst = connectwire.AppendRaw(dst, 23, m.ModelFamilyUIDView)
	dst = connectwire.AppendRaw(dst, 24, m.InferenceConfigView)
	return connectwire.AppendBool(dst, 25, m.ModelRouter)
}

func ConsumeModelInfo(b []byte, m *ModelInfo) error {
	return connectwire.ConsumeFields(b, "devin model info", func(f connectwire.Field) error {
		return consumeModelInfoField(f, m)
	})
}

func consumeModelInfoField(f connectwire.Field, m *ModelInfo) error {
	switch {
	case f.Number <= 6:
		return consumeModelIdentity(f, m)
	case f.Number <= 12:
		return consumeModelEndpoints(f, m)
	case f.Number <= 18:
		return consumeModelFormatting(f, m)
	default:
		return consumeModelDeployment(f, m)
	}
}

func consumeModelIdentity(f connectwire.Field, m *ModelInfo) error {
	switch f.Number {
	case 1:
		return connectwire.SetInt32(f, "devin model info", &m.ModelID)
	case 2:
		return connectwire.SetBool(f, "devin model info", &m.Internal)
	case 3:
		return connectwire.SetInt32(f, "devin model info", &m.ModelType)
	case 4:
		return connectwire.SetInt32(f, "devin model info", &m.MaxTokens)
	case 5:
		return connectwire.SetBytes(f, "devin model info", &m.TokenizerTypeView)
	case 6:
		return connectwire.SetBytes(f, "devin model info", &m.ModelFeaturesView)
	default:
		return nil
	}
}

func consumeModelEndpoints(f connectwire.Field, m *ModelInfo) error {
	switch f.Number {
	case 7:
		return connectwire.SetInt32(f, "devin model info", &m.APIProvider)
	case 8:
		return connectwire.SetBytes(f, "devin model info", &m.ModelNameView)
	case 9:
		return connectwire.SetBool(f, "devin model info", &m.SupportsContext)
	case 10:
		return connectwire.SetInt32(f, "devin model info", &m.EmbedDim)
	case 11:
		return connectwire.SetBytes(f, "devin model info", &m.BaseURLView)
	case 12:
		return connectwire.SetBytes(f, "devin model info", &m.ChatModelNameView)
	default:
		return nil
	}
}

func consumeModelFormatting(f connectwire.Field, m *ModelInfo) error {
	switch f.Number {
	case 13:
		return connectwire.SetInt32(f, "devin model info", &m.MaxOutputTokens)
	case 14:
		return connectwire.SetInt32(f, "devin model info", &m.PromptTemplaterType)
	case 15:
		return connectwire.SetInt32(f, "devin model info", &m.ToolFormatterType)
	case 17:
		return connectwire.SetBytes(f, "devin model info", &m.ModelUIDView)
	case 18:
		return connectwire.SetBytes(f, "devin model info", &m.InferenceServerURLView)
	default:
		return nil
	}
}

func consumeModelDeployment(f connectwire.Field, m *ModelInfo) error {
	switch f.Number {
	case 20:
		return connectwire.AppendBytesField(f, "devin model info", &m.HarnessUIDs)
	case 21:
		return connectwire.SetBytes(f, "devin model info", &m.ArenaConfigView)
	case 22:
		return connectwire.SetInt32(f, "devin model info", &m.DisplayOption)
	case 23:
		return connectwire.SetBytes(f, "devin model info", &m.ModelFamilyUIDView)
	case 24:
		return connectwire.SetBytes(f, "devin model info", &m.InferenceConfigView)
	case 25:
		return connectwire.SetBool(f, "devin model info", &m.ModelRouter)
	default:
		return nil
	}
}

type ClientModelConfig struct {
	LabelView               []byte
	ModelOrAliasView        []byte
	ModelUIDView            []byte
	CreditMultiplier        float32
	PricingType             int32
	Disabled                bool
	SupportsImages          bool
	SupportsLegacy          bool
	Premium                 bool
	BetaWarningMessageView  []byte
	Beta                    bool
	Provider                int32
	Recommended             bool
	AllowedTiers            []int32
	APIProvider             int32
	New                     bool
	PartialRollout          bool
	RolloutFraction         float32
	ContextWindow           int32
	PromoStatusView         []byte
	CapacityLimited         bool
	FastStatusView          []byte
	ModelInfo               *ModelInfo
	ModelCostTier           int32
	DescriptionView         []byte
	SmartFriendModelUIDView []byte
	ModelFamilyMetadataView []byte
	DefaultModelInFamily    bool
	ModelDimensions         [][]byte
	DisabledReasonView      []byte
}

func AppendClientModelConfig(dst []byte, m *ClientModelConfig) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.LabelView)
	dst = connectwire.AppendRaw(dst, 2, m.ModelOrAliasView)
	dst = appendFloat32(dst, 3, m.CreditMultiplier)
	dst = connectwire.AppendBool(dst, 4, m.Disabled)
	dst = connectwire.AppendBool(dst, 5, m.SupportsImages)
	dst = connectwire.AppendBool(dst, 6, m.SupportsLegacy)
	dst = connectwire.AppendBool(dst, 7, m.Premium)
	dst = connectwire.AppendRaw(dst, 8, m.BetaWarningMessageView)
	dst = connectwire.AppendBool(dst, 9, m.Beta)
	dst = connectwire.AppendInt32(dst, 10, m.Provider)
	dst = connectwire.AppendBool(dst, 11, m.Recommended)
	dst = appendPackedInt32(dst, 12, m.AllowedTiers)
	dst = connectwire.AppendInt32(dst, 13, m.PricingType)
	dst = connectwire.AppendInt32(dst, 14, m.APIProvider)
	dst = connectwire.AppendBool(dst, 15, m.New)
	dst = connectwire.AppendBool(dst, 16, m.PartialRollout)
	dst = appendFloat32(dst, 17, m.RolloutFraction)
	dst = connectwire.AppendInt32(dst, 18, m.ContextWindow)
	dst = connectwire.AppendRaw(dst, 19, m.PromoStatusView)
	dst = connectwire.AppendBool(dst, 20, m.CapacityLimited)
	dst = connectwire.AppendRaw(dst, 21, m.FastStatusView)
	dst = connectwire.AppendRaw(dst, 22, m.ModelUIDView)
	if m.ModelInfo != nil {
		dst = connectwire.AppendMessage(dst, 23, m.ModelInfo, AppendModelInfo)
	}
	dst = connectwire.AppendInt32(dst, 24, m.ModelCostTier)
	dst = connectwire.AppendRaw(dst, 27, m.DescriptionView)
	dst = connectwire.AppendRaw(dst, 29, m.SmartFriendModelUIDView)
	dst = connectwire.AppendRaw(dst, 30, m.ModelFamilyMetadataView)
	dst = connectwire.AppendBool(dst, 31, m.DefaultModelInFamily)
	dst = connectwire.AppendRepeatedBytes(dst, 32, m.ModelDimensions)
	return connectwire.AppendRaw(dst, 33, m.DisabledReasonView)
}

func ConsumeClientModelConfig(b []byte, m *ClientModelConfig) error {
	return connectwire.ConsumeFields(b, "devin model config", func(f connectwire.Field) error {
		return consumeClientModelConfigField(f, m)
	})
}

func consumeClientModelConfigField(f connectwire.Field, m *ClientModelConfig) error {
	switch {
	case f.Number <= 6:
		return consumeClientModelCore(f, m)
	case f.Number <= 12:
		return consumeClientModelTier(f, m)
	case f.Number <= 18:
		return consumeClientModelRollout(f, m)
	case f.Number <= 24:
		return consumeClientModelAvailability(f, m)
	default:
		return consumeClientModelFamily(f, m)
	}
}

func consumeClientModelCore(f connectwire.Field, m *ClientModelConfig) error {
	switch f.Number {
	case 1:
		return connectwire.SetBytes(f, "devin model config", &m.LabelView)
	case 2:
		return connectwire.SetBytes(f, "devin model config", &m.ModelOrAliasView)
	case 3:
		return setFloat32(f, "devin model config", &m.CreditMultiplier)
	case 4:
		return connectwire.SetBool(f, "devin model config", &m.Disabled)
	case 5:
		return connectwire.SetBool(f, "devin model config", &m.SupportsImages)
	case 6:
		return connectwire.SetBool(f, "devin model config", &m.SupportsLegacy)
	default:
		return nil
	}
}

func consumeClientModelTier(f connectwire.Field, m *ClientModelConfig) error {
	switch f.Number {
	case 7:
		return connectwire.SetBool(f, "devin model config", &m.Premium)
	case 8:
		return connectwire.SetBytes(f, "devin model config", &m.BetaWarningMessageView)
	case 9:
		return connectwire.SetBool(f, "devin model config", &m.Beta)
	case 10:
		return connectwire.SetInt32(f, "devin model config", &m.Provider)
	case 11:
		return connectwire.SetBool(f, "devin model config", &m.Recommended)
	case 12:
		return consumeAllowedTier(f, m)
	default:
		return nil
	}
}

func consumeClientModelRollout(f connectwire.Field, m *ClientModelConfig) error {
	switch f.Number {
	case 13:
		return connectwire.SetInt32(f, "devin model config", &m.PricingType)
	case 14:
		return connectwire.SetInt32(f, "devin model config", &m.APIProvider)
	case 15:
		return connectwire.SetBool(f, "devin model config", &m.New)
	case 16:
		return connectwire.SetBool(f, "devin model config", &m.PartialRollout)
	case 17:
		return setFloat32(f, "devin model config", &m.RolloutFraction)
	case 18:
		return connectwire.SetInt32(f, "devin model config", &m.ContextWindow)
	default:
		return nil
	}
}

func consumeClientModelAvailability(f connectwire.Field, m *ClientModelConfig) error {
	switch f.Number {
	case 19:
		return connectwire.SetBytes(f, "devin model config", &m.PromoStatusView)
	case 20:
		return connectwire.SetBool(f, "devin model config", &m.CapacityLimited)
	case 21:
		return connectwire.SetBytes(f, "devin model config", &m.FastStatusView)
	case 22:
		return connectwire.SetBytes(f, "devin model config", &m.ModelUIDView)
	case 23:
		return connectwire.ConsumeNested(f, "devin model config", &m.ModelInfo, ConsumeModelInfo)
	case 24:
		return connectwire.SetInt32(f, "devin model config", &m.ModelCostTier)
	default:
		return nil
	}
}

func consumeClientModelFamily(f connectwire.Field, m *ClientModelConfig) error {
	switch f.Number {
	case 27:
		return connectwire.SetBytes(f, "devin model config", &m.DescriptionView)
	case 29:
		return connectwire.SetBytes(f, "devin model config", &m.SmartFriendModelUIDView)
	case 30:
		return connectwire.SetBytes(f, "devin model config", &m.ModelFamilyMetadataView)
	case 31:
		return connectwire.SetBool(f, "devin model config", &m.DefaultModelInFamily)
	case 32:
		return connectwire.AppendBytesField(f, "devin model config", &m.ModelDimensions)
	case 33:
		return connectwire.SetBytes(f, "devin model config", &m.DisabledReasonView)
	default:
		return nil
	}
}

func consumeAllowedTier(f connectwire.Field, m *ClientModelConfig) error {
	if f.Kind == protowire.VarintType {
		m.AllowedTiers = append(m.AllowedTiers, connectwire.Signed32(f))
		return nil
	}
	if f.Kind != protowire.BytesType {
		return connectwire.RequireKind(f, protowire.VarintType, "devin model config")
	}
	packed := connectwire.BytesValue(f)
	for len(packed) > 0 {
		_, size := protowire.ConsumeVarint(packed)
		if size < 0 {
			return protowire.ParseError(size)
		}
		m.AllowedTiers = append(m.AllowedTiers, connectwire.Signed32(connectwire.Field{Raw: packed[:size]}))
		packed = packed[size:]
	}
	return nil
}

type ModelConfigRequest struct {
	Metadata *Metadata
}

type GetCliModelConfigsRequest = ModelConfigRequest

func AppendModelConfigRequest(dst []byte, m *ModelConfigRequest) []byte {
	return connectwire.AppendOptionalMessage(dst, 1, m.Metadata, AppendMetadata)
}

func ConsumeModelConfigRequest(b []byte, m *ModelConfigRequest) error {
	return connectwire.ConsumeFields(b, "devin model config request", func(f connectwire.Field) error {
		if f.Number != 1 {
			return nil
		}
		return connectwire.ConsumeNested(f, "devin model config request", &m.Metadata, ConsumeMetadata)
	})
}

type ModelConfigResponse struct {
	Models                    []ClientModelConfig
	DefaultOverrideConfigView []byte
}

type GetCliModelConfigsResponse = ModelConfigResponse

func AppendModelConfigResponse(dst []byte, m *ModelConfigResponse) []byte {
	for i := range m.Models {
		dst = connectwire.AppendMessage(dst, 1, &m.Models[i], AppendClientModelConfig)
	}
	return connectwire.AppendRaw(dst, 3, m.DefaultOverrideConfigView)
}

func ConsumeModelConfigResponse(b []byte, m *ModelConfigResponse) error {
	return connectwire.ConsumeFields(b, "devin model config response", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.ConsumeRepeated(f, "devin model config response", &m.Models, ConsumeClientModelConfig)
		case 3:
			return connectwire.SetBytes(f, "devin model config response", &m.DefaultOverrideConfigView)
		default:
			return nil
		}
	})
}

type GetUserJWTRequest struct {
	Metadata *Metadata
}

func AppendGetUserJWTRequest(dst []byte, m *GetUserJWTRequest) []byte {
	return connectwire.AppendOptionalMessage(dst, 1, m.Metadata, AppendMetadata)
}

func ConsumeGetUserJWTRequest(b []byte, m *GetUserJWTRequest) error {
	return connectwire.ConsumeFields(b, "devin JWT request", func(f connectwire.Field) error {
		if f.Number != 1 {
			return nil
		}
		return connectwire.ConsumeNested(f, "devin JWT request", &m.Metadata, ConsumeMetadata)
	})
}

type GetUserJWTResponse struct {
	UserJWTView            []byte
	CustomAPIServerURLView []byte
}

func AppendGetUserJWTResponse(dst []byte, m *GetUserJWTResponse) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.UserJWTView)
	return connectwire.AppendRaw(dst, 2, m.CustomAPIServerURLView)
}

func ConsumeGetUserJWTResponse(b []byte, m *GetUserJWTResponse) error {
	return connectwire.ConsumeFields(b, "devin JWT response", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "devin JWT response", &m.UserJWTView)
		case 2:
			return connectwire.SetBytes(f, "devin JWT response", &m.CustomAPIServerURLView)
		default:
			return nil
		}
	})
}
