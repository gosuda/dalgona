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
		switch {
		case f.Number <= 5:
			return consumeModelInfoBase(f, m)
		case f.Number <= 9:
			return consumeModelInfoFeatures(f, m)
		case f.Number <= 18:
			return consumeModelInfoLimits(f, m)
		default:
			return consumeModelInfoRouting(f, m)
		}
	})
}

func consumeModelInfoBase(f connectwire.Field, m *ModelInfo) error {
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
	}
	return nil
}

func consumeModelInfoFeatures(f connectwire.Field, m *ModelInfo) error {
	switch f.Number {
	case 6:
		return connectwire.SetBytes(f, "devin model info", &m.ModelFeaturesView)
	case 7:
		return connectwire.SetInt32(f, "devin model info", &m.APIProvider)
	case 8:
		return connectwire.SetBytes(f, "devin model info", &m.ModelNameView)
	case 9:
		return connectwire.SetBool(f, "devin model info", &m.SupportsContext)
	}
	return nil
}

func consumeModelInfoLimits(f connectwire.Field, m *ModelInfo) error {
	switch f.Number {
	case 10:
		return connectwire.SetInt32(f, "devin model info", &m.EmbedDim)
	case 11:
		return connectwire.SetBytes(f, "devin model info", &m.BaseURLView)
	case 12:
		return connectwire.SetBytes(f, "devin model info", &m.ChatModelNameView)
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
	}
	return nil
}

func consumeModelInfoRouting(f connectwire.Field, m *ModelInfo) error {
	switch f.Number {
	case 20:
		if err := connectwire.RequireKind(f, protowire.BytesType, "devin model info"); err != nil {
			return err
		}
		m.HarnessUIDs = append(m.HarnessUIDs, connectwire.BytesValue(f))
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
	}
	return nil
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
		switch {
		case f.Number <= 5:
			return consumeModelConfigIdentity(f, m)
		case f.Number <= 10:
			return consumeModelConfigBase(f, m)
		case f.Number <= 16:
			return consumeModelConfigFlags(f, m)
		case f.Number <= 20:
			return consumeModelConfigLimits(f, m)
		case f.Number <= 27:
			return consumeModelConfigMetadata(f, m)
		default:
			return consumeModelConfigExtra(f, m)
		}
	})
}

func consumeModelConfigIdentity(f connectwire.Field, m *ClientModelConfig) error {
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
	}
	return nil
}

func consumeModelConfigBase(f connectwire.Field, m *ClientModelConfig) error {
	switch f.Number {
	case 6:
		return connectwire.SetBool(f, "devin model config", &m.SupportsLegacy)
	case 7:
		return connectwire.SetBool(f, "devin model config", &m.Premium)
	case 8:
		return connectwire.SetBytes(f, "devin model config", &m.BetaWarningMessageView)
	case 9:
		return connectwire.SetBool(f, "devin model config", &m.Beta)
	case 10:
		return connectwire.SetInt32(f, "devin model config", &m.Provider)
	}
	return nil
}

func consumeModelConfigFlags(f connectwire.Field, m *ClientModelConfig) error {
	switch f.Number {
	case 11:
		return connectwire.SetBool(f, "devin model config", &m.Recommended)
	case 12:
		return consumeAllowedTier(f, m)
	case 13:
		return connectwire.SetInt32(f, "devin model config", &m.PricingType)
	case 14:
		return connectwire.SetInt32(f, "devin model config", &m.APIProvider)
	case 15:
		return connectwire.SetBool(f, "devin model config", &m.New)
	case 16:
		return connectwire.SetBool(f, "devin model config", &m.PartialRollout)
	}
	return nil
}

func consumeModelConfigLimits(f connectwire.Field, m *ClientModelConfig) error {
	switch f.Number {
	case 17:
		return setFloat32(f, "devin model config", &m.RolloutFraction)
	case 18:
		return connectwire.SetInt32(f, "devin model config", &m.ContextWindow)
	case 19:
		return connectwire.SetBytes(f, "devin model config", &m.PromoStatusView)
	case 20:
		return connectwire.SetBool(f, "devin model config", &m.CapacityLimited)
	}
	return nil
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

func consumeModelConfigMetadata(f connectwire.Field, m *ClientModelConfig) error {
	switch f.Number {
	case 21:
		return connectwire.SetBytes(f, "devin model config", &m.FastStatusView)
	case 22:
		return connectwire.SetBytes(f, "devin model config", &m.ModelUIDView)
	case 23:
		if err := connectwire.RequireKind(f, protowire.BytesType, "devin model config"); err != nil {
			return err
		}
		m.ModelInfo = new(ModelInfo)
		return ConsumeModelInfo(connectwire.BytesValue(f), m.ModelInfo)
	case 24:
		return connectwire.SetInt32(f, "devin model config", &m.ModelCostTier)
	case 27:
		return connectwire.SetBytes(f, "devin model config", &m.DescriptionView)
	}
	return nil
}

func consumeModelConfigExtra(f connectwire.Field, m *ClientModelConfig) error {
	switch f.Number {
	case 29:
		return connectwire.SetBytes(f, "devin model config", &m.SmartFriendModelUIDView)
	case 30:
		return connectwire.SetBytes(f, "devin model config", &m.ModelFamilyMetadataView)
	case 31:
		return connectwire.SetBool(f, "devin model config", &m.DefaultModelInFamily)
	case 32:
		if err := connectwire.RequireKind(f, protowire.BytesType, "devin model config"); err != nil {
			return err
		}
		m.ModelDimensions = append(m.ModelDimensions, connectwire.BytesValue(f))
	case 33:
		return connectwire.SetBytes(f, "devin model config", &m.DisabledReasonView)
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
			if err := connectwire.RequireKind(f, protowire.BytesType, "devin model config response"); err != nil {
				return err
			}
			var model ClientModelConfig
			if err := ConsumeClientModelConfig(connectwire.BytesValue(f), &model); err != nil {
				return err
			}
			m.Models = append(m.Models, model)
		case 3:
			return connectwire.SetBytes(f, "devin model config response", &m.DefaultOverrideConfigView)
		}
		return nil
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
