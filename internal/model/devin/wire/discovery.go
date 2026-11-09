package wire

import "google.golang.org/protobuf/encoding/protowire"

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
	dst = appendInt32(dst, 1, m.ModelID)
	dst = appendBool(dst, 2, m.Internal)
	dst = appendInt32(dst, 3, m.ModelType)
	dst = appendInt32(dst, 4, m.MaxTokens)
	dst = appendRaw(dst, 5, m.TokenizerTypeView)
	dst = appendRaw(dst, 6, m.ModelFeaturesView)
	dst = appendInt32(dst, 7, m.APIProvider)
	dst = appendRaw(dst, 8, m.ModelNameView)
	dst = appendBool(dst, 9, m.SupportsContext)
	dst = appendInt32(dst, 10, m.EmbedDim)
	dst = appendRaw(dst, 11, m.BaseURLView)
	dst = appendRaw(dst, 12, m.ChatModelNameView)
	dst = appendInt32(dst, 13, m.MaxOutputTokens)
	dst = appendInt32(dst, 14, m.PromptTemplaterType)
	dst = appendInt32(dst, 15, m.ToolFormatterType)
	dst = appendRaw(dst, 17, m.ModelUIDView)
	dst = appendRaw(dst, 18, m.InferenceServerURLView)
	dst = appendRepeatedBytes(dst, 20, m.HarnessUIDs)
	dst = appendRaw(dst, 21, m.ArenaConfigView)
	dst = appendInt32(dst, 22, m.DisplayOption)
	dst = appendRaw(dst, 23, m.ModelFamilyUIDView)
	dst = appendRaw(dst, 24, m.InferenceConfigView)
	return appendBool(dst, 25, m.ModelRouter)
}

func ConsumeModelInfo(b []byte, m *ModelInfo) error {
	return consumeFields(b, "devin model info", func(f field) error {
		switch {
		case f.number <= 5:
			return consumeModelInfoBase(f, m)
		case f.number <= 9:
			return consumeModelInfoFeatures(f, m)
		case f.number <= 18:
			return consumeModelInfoLimits(f, m)
		default:
			return consumeModelInfoRouting(f, m)
		}
	})
}

func consumeModelInfoBase(f field, m *ModelInfo) error {
	switch f.number {
	case 1:
		return setInt32(f, "devin model info", &m.ModelID)
	case 2:
		return setBool(f, "devin model info", &m.Internal)
	case 3:
		return setInt32(f, "devin model info", &m.ModelType)
	case 4:
		return setInt32(f, "devin model info", &m.MaxTokens)
	case 5:
		return setBytes(f, "devin model info", &m.TokenizerTypeView)
	}
	return nil
}

func consumeModelInfoFeatures(f field, m *ModelInfo) error {
	switch f.number {
	case 6:
		return setBytes(f, "devin model info", &m.ModelFeaturesView)
	case 7:
		return setInt32(f, "devin model info", &m.APIProvider)
	case 8:
		return setBytes(f, "devin model info", &m.ModelNameView)
	case 9:
		return setBool(f, "devin model info", &m.SupportsContext)
	}
	return nil
}

func consumeModelInfoLimits(f field, m *ModelInfo) error {
	switch f.number {
	case 10:
		return setInt32(f, "devin model info", &m.EmbedDim)
	case 11:
		return setBytes(f, "devin model info", &m.BaseURLView)
	case 12:
		return setBytes(f, "devin model info", &m.ChatModelNameView)
	case 13:
		return setInt32(f, "devin model info", &m.MaxOutputTokens)
	case 14:
		return setInt32(f, "devin model info", &m.PromptTemplaterType)
	case 15:
		return setInt32(f, "devin model info", &m.ToolFormatterType)
	case 17:
		return setBytes(f, "devin model info", &m.ModelUIDView)
	case 18:
		return setBytes(f, "devin model info", &m.InferenceServerURLView)
	}
	return nil
}

func consumeModelInfoRouting(f field, m *ModelInfo) error {
	switch f.number {
	case 20:
		if err := requireKind(f, protowire.BytesType, "devin model info"); err != nil {
			return err
		}
		m.HarnessUIDs = append(m.HarnessUIDs, bytesValue(f))
	case 21:
		return setBytes(f, "devin model info", &m.ArenaConfigView)
	case 22:
		return setInt32(f, "devin model info", &m.DisplayOption)
	case 23:
		return setBytes(f, "devin model info", &m.ModelFamilyUIDView)
	case 24:
		return setBytes(f, "devin model info", &m.InferenceConfigView)
	case 25:
		return setBool(f, "devin model info", &m.ModelRouter)
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
	dst = appendRaw(dst, 1, m.LabelView)
	dst = appendRaw(dst, 2, m.ModelOrAliasView)
	dst = appendFloat32(dst, 3, m.CreditMultiplier)
	dst = appendBool(dst, 4, m.Disabled)
	dst = appendBool(dst, 5, m.SupportsImages)
	dst = appendBool(dst, 6, m.SupportsLegacy)
	dst = appendBool(dst, 7, m.Premium)
	dst = appendRaw(dst, 8, m.BetaWarningMessageView)
	dst = appendBool(dst, 9, m.Beta)
	dst = appendInt32(dst, 10, m.Provider)
	dst = appendBool(dst, 11, m.Recommended)
	dst = appendPackedInt32(dst, 12, m.AllowedTiers)
	dst = appendInt32(dst, 13, m.PricingType)
	dst = appendInt32(dst, 14, m.APIProvider)
	dst = appendBool(dst, 15, m.New)
	dst = appendBool(dst, 16, m.PartialRollout)
	dst = appendFloat32(dst, 17, m.RolloutFraction)
	dst = appendInt32(dst, 18, m.ContextWindow)
	dst = appendRaw(dst, 19, m.PromoStatusView)
	dst = appendBool(dst, 20, m.CapacityLimited)
	dst = appendRaw(dst, 21, m.FastStatusView)
	dst = appendRaw(dst, 22, m.ModelUIDView)
	if m.ModelInfo != nil {
		dst = appendMessage(dst, 23, m.ModelInfo, AppendModelInfo)
	}
	dst = appendInt32(dst, 24, m.ModelCostTier)
	dst = appendRaw(dst, 27, m.DescriptionView)
	dst = appendRaw(dst, 29, m.SmartFriendModelUIDView)
	dst = appendRaw(dst, 30, m.ModelFamilyMetadataView)
	dst = appendBool(dst, 31, m.DefaultModelInFamily)
	dst = appendRepeatedBytes(dst, 32, m.ModelDimensions)
	return appendRaw(dst, 33, m.DisabledReasonView)
}

func ConsumeClientModelConfig(b []byte, m *ClientModelConfig) error {
	return consumeFields(b, "devin model config", func(f field) error {
		switch {
		case f.number <= 5:
			return consumeModelConfigIdentity(f, m)
		case f.number <= 10:
			return consumeModelConfigBase(f, m)
		case f.number <= 16:
			return consumeModelConfigFlags(f, m)
		case f.number <= 20:
			return consumeModelConfigLimits(f, m)
		case f.number <= 27:
			return consumeModelConfigMetadata(f, m)
		default:
			return consumeModelConfigExtra(f, m)
		}
	})
}

func consumeModelConfigIdentity(f field, m *ClientModelConfig) error {
	switch f.number {
	case 1:
		return setBytes(f, "devin model config", &m.LabelView)
	case 2:
		return setBytes(f, "devin model config", &m.ModelOrAliasView)
	case 3:
		return setFloat32(f, "devin model config", &m.CreditMultiplier)
	case 4:
		return setBool(f, "devin model config", &m.Disabled)
	case 5:
		return setBool(f, "devin model config", &m.SupportsImages)
	}
	return nil
}

func consumeModelConfigBase(f field, m *ClientModelConfig) error {
	switch f.number {
	case 6:
		return setBool(f, "devin model config", &m.SupportsLegacy)
	case 7:
		return setBool(f, "devin model config", &m.Premium)
	case 8:
		return setBytes(f, "devin model config", &m.BetaWarningMessageView)
	case 9:
		return setBool(f, "devin model config", &m.Beta)
	case 10:
		return setInt32(f, "devin model config", &m.Provider)
	}
	return nil
}

func consumeModelConfigFlags(f field, m *ClientModelConfig) error {
	switch f.number {
	case 11:
		return setBool(f, "devin model config", &m.Recommended)
	case 12:
		return consumeAllowedTier(f, m)
	case 13:
		return setInt32(f, "devin model config", &m.PricingType)
	case 14:
		return setInt32(f, "devin model config", &m.APIProvider)
	case 15:
		return setBool(f, "devin model config", &m.New)
	case 16:
		return setBool(f, "devin model config", &m.PartialRollout)
	}
	return nil
}

func consumeModelConfigLimits(f field, m *ClientModelConfig) error {
	switch f.number {
	case 17:
		return setFloat32(f, "devin model config", &m.RolloutFraction)
	case 18:
		return setInt32(f, "devin model config", &m.ContextWindow)
	case 19:
		return setBytes(f, "devin model config", &m.PromoStatusView)
	case 20:
		return setBool(f, "devin model config", &m.CapacityLimited)
	}
	return nil
}

func consumeAllowedTier(f field, m *ClientModelConfig) error {
	if f.kind == protowire.VarintType {
		m.AllowedTiers = append(m.AllowedTiers, signed32(f))
		return nil
	}
	if f.kind != protowire.BytesType {
		return requireKind(f, protowire.VarintType, "devin model config")
	}
	packed := bytesValue(f)
	for len(packed) > 0 {
		_, size := protowire.ConsumeVarint(packed)
		if size < 0 {
			return protowire.ParseError(size)
		}
		m.AllowedTiers = append(m.AllowedTiers, signed32(field{raw: packed[:size]}))
		packed = packed[size:]
	}
	return nil
}

func consumeModelConfigMetadata(f field, m *ClientModelConfig) error {
	switch f.number {
	case 21:
		return setBytes(f, "devin model config", &m.FastStatusView)
	case 22:
		return setBytes(f, "devin model config", &m.ModelUIDView)
	case 23:
		if err := requireKind(f, protowire.BytesType, "devin model config"); err != nil {
			return err
		}
		m.ModelInfo = new(ModelInfo)
		return ConsumeModelInfo(bytesValue(f), m.ModelInfo)
	case 24:
		return setInt32(f, "devin model config", &m.ModelCostTier)
	case 27:
		return setBytes(f, "devin model config", &m.DescriptionView)
	}
	return nil
}

func consumeModelConfigExtra(f field, m *ClientModelConfig) error {
	switch f.number {
	case 29:
		return setBytes(f, "devin model config", &m.SmartFriendModelUIDView)
	case 30:
		return setBytes(f, "devin model config", &m.ModelFamilyMetadataView)
	case 31:
		return setBool(f, "devin model config", &m.DefaultModelInFamily)
	case 32:
		if err := requireKind(f, protowire.BytesType, "devin model config"); err != nil {
			return err
		}
		m.ModelDimensions = append(m.ModelDimensions, bytesValue(f))
	case 33:
		return setBytes(f, "devin model config", &m.DisabledReasonView)
	}
	return nil
}

type ModelConfigRequest struct {
	Metadata *Metadata
}

type GetCliModelConfigsRequest = ModelConfigRequest

func AppendModelConfigRequest(dst []byte, m *ModelConfigRequest) []byte {
	return appendOptionalMessage(dst, 1, m.Metadata, AppendMetadata)
}

func ConsumeModelConfigRequest(b []byte, m *ModelConfigRequest) error {
	return consumeFields(b, "devin model config request", func(f field) error {
		if f.number != 1 {
			return nil
		}
		return consumeNested(f, "devin model config request", &m.Metadata, ConsumeMetadata)
	})
}

func AppendGetCliModelConfigsRequest(dst []byte, m *GetCliModelConfigsRequest) []byte {
	return AppendModelConfigRequest(dst, m)
}

func ConsumeGetCliModelConfigsRequest(b []byte, m *GetCliModelConfigsRequest) error {
	return ConsumeModelConfigRequest(b, m)
}

type ModelConfigResponse struct {
	Models                    []ClientModelConfig
	DefaultOverrideConfigView []byte
}

type GetCliModelConfigsResponse = ModelConfigResponse

func AppendModelConfigResponse(dst []byte, m *ModelConfigResponse) []byte {
	for i := range m.Models {
		dst = appendMessage(dst, 1, &m.Models[i], AppendClientModelConfig)
	}
	return appendRaw(dst, 3, m.DefaultOverrideConfigView)
}

func ConsumeModelConfigResponse(b []byte, m *ModelConfigResponse) error {
	return consumeFields(b, "devin model config response", func(f field) error {
		switch f.number {
		case 1:
			if err := requireKind(f, protowire.BytesType, "devin model config response"); err != nil {
				return err
			}
			var model ClientModelConfig
			if err := ConsumeClientModelConfig(bytesValue(f), &model); err != nil {
				return err
			}
			m.Models = append(m.Models, model)
		case 3:
			return setBytes(f, "devin model config response", &m.DefaultOverrideConfigView)
		}
		return nil
	})
}

func AppendGetCliModelConfigsResponse(dst []byte, m *GetCliModelConfigsResponse) []byte {
	return AppendModelConfigResponse(dst, m)
}

func ConsumeGetCliModelConfigsResponse(b []byte, m *GetCliModelConfigsResponse) error {
	return ConsumeModelConfigResponse(b, m)
}

type GetUserJWTRequest struct {
	Metadata *Metadata
}

func AppendGetUserJWTRequest(dst []byte, m *GetUserJWTRequest) []byte {
	return appendOptionalMessage(dst, 1, m.Metadata, AppendMetadata)
}

func ConsumeGetUserJWTRequest(b []byte, m *GetUserJWTRequest) error {
	return consumeFields(b, "devin JWT request", func(f field) error {
		if f.number != 1 {
			return nil
		}
		return consumeNested(f, "devin JWT request", &m.Metadata, ConsumeMetadata)
	})
}

type GetUserJWTResponse struct {
	UserJWTView            []byte
	CustomAPIServerURLView []byte
}

func AppendGetUserJWTResponse(dst []byte, m *GetUserJWTResponse) []byte {
	dst = appendRaw(dst, 1, m.UserJWTView)
	return appendRaw(dst, 2, m.CustomAPIServerURLView)
}

func ConsumeGetUserJWTResponse(b []byte, m *GetUserJWTResponse) error {
	return consumeFields(b, "devin JWT response", func(f field) error {
		switch f.number {
		case 1:
			return setBytes(f, "devin JWT response", &m.UserJWTView)
		case 2:
			return setBytes(f, "devin JWT response", &m.CustomAPIServerURLView)
		default:
			return nil
		}
	})
}
