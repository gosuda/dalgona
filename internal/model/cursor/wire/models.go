package wire

import (
	"google.golang.org/protobuf/encoding/protowire"
)

type ModelParameter struct {
	IDView    []byte
	ValueView []byte
}

func AppendModelParameter(dst []byte, m *ModelParameter) []byte {
	dst = appendRaw(dst, 1, m.IDView)
	return appendRaw(dst, 2, m.ValueView)
}

func ConsumeModelParameter(b []byte, m *ModelParameter) error {
	return consumeFields(b, "cursor model parameter", func(f field) error {
		switch f.number {
		case 1:
			return setBytes(f, "cursor model parameter", &m.IDView)
		case 2:
			return setBytes(f, "cursor model parameter", &m.ValueView)
		default:
			return nil
		}
	})
}

type RequestedModel struct {
	ModelIDView      []byte
	MaxMode          bool
	Parameters       []ModelParameter
	CredentialNumber int32
	CredentialsView  []byte
}

func AppendRequestedModel(dst []byte, m *RequestedModel) []byte {
	dst = appendRaw(dst, 1, m.ModelIDView)
	dst = appendBool(dst, 2, m.MaxMode)
	for i := range m.Parameters {
		dst = appendMessage(dst, 3, &m.Parameters[i], AppendModelParameter)
	}
	if m.CredentialNumber != 0 {
		dst = appendVariant(dst, protowire.Number(m.CredentialNumber), m.CredentialsView)
	}
	return dst
}

func ConsumeRequestedModel(b []byte, m *RequestedModel) error {
	return consumeFields(b, "cursor requested model", func(f field) error {
		switch f.number {
		case 1:
			return setBytes(f, "cursor requested model", &m.ModelIDView)
		case 2:
			return setBool(f, "cursor requested model", &m.MaxMode)
		case 3:
			if err := requireKind(f, protowire.BytesType, "cursor requested model"); err != nil {
				return err
			}
			var p ModelParameter
			if err := ConsumeModelParameter(bytesValue(f), &p); err != nil {
				return err
			}
			m.Parameters = append(m.Parameters, p)
		case 4, 5, 6:
			m.CredentialNumber = f.number
			return setBytes(f, "cursor requested model", &m.CredentialsView)
		}
		return nil
	})
}

type ModelDetails struct {
	ModelIDView         []byte
	ThinkingDetailsView []byte
	DisplayModelIDView  []byte
	DisplayNameView     []byte
	DisplayShortView    []byte
	Aliases             [][]byte
	MaxMode             bool
	CredentialNumber    int32
	CredentialsView     []byte
}

func AppendModelDetails(dst []byte, m *ModelDetails) []byte {
	dst = appendRaw(dst, 1, m.ModelIDView)
	dst = appendRaw(dst, 2, m.ThinkingDetailsView)
	dst = appendRaw(dst, 3, m.DisplayModelIDView)
	dst = appendRaw(dst, 4, m.DisplayNameView)
	dst = appendRaw(dst, 5, m.DisplayShortView)
	dst = appendRepeatedBytes(dst, 6, m.Aliases)
	dst = appendBool(dst, 7, m.MaxMode)
	if m.CredentialNumber != 0 {
		dst = appendVariant(dst, protowire.Number(m.CredentialNumber), m.CredentialsView)
	}
	return dst
}

func ConsumeModelDetails(b []byte, m *ModelDetails) error {
	return consumeFields(b, "cursor model details", func(f field) error {
		if f.number <= 5 {
			return consumeModelIdentity(f, m)
		}
		return consumeModelOptions(f, m)
	})
}

func consumeModelIdentity(f field, m *ModelDetails) error {
	switch f.number {
	case 1:
		return setBytes(f, "cursor model details", &m.ModelIDView)
	case 2:
		return setBytes(f, "cursor model details", &m.ThinkingDetailsView)
	case 3:
		return setBytes(f, "cursor model details", &m.DisplayModelIDView)
	case 4:
		return setBytes(f, "cursor model details", &m.DisplayNameView)
	case 5:
		return setBytes(f, "cursor model details", &m.DisplayShortView)
	default:
		return nil
	}
}

func consumeModelOptions(f field, m *ModelDetails) error {
	switch f.number {
	case 6:
		if err := requireKind(f, protowire.BytesType, "cursor model details"); err != nil {
			return err
		}
		m.Aliases = append(m.Aliases, bytesValue(f))
	case 7:
		return setBool(f, "cursor model details", &m.MaxMode)
	case 8, 9, 10:
		m.CredentialNumber = f.number
		return setBytes(f, "cursor model details", &m.CredentialsView)
	}
	return nil
}

type GetUsableModelsRequest struct {
	CustomModelIDs [][]byte
}

func AppendGetUsableModelsRequest(dst []byte, m *GetUsableModelsRequest) []byte {
	return appendRepeatedBytes(dst, 1, m.CustomModelIDs)
}

func ConsumeGetUsableModelsRequest(b []byte, m *GetUsableModelsRequest) error {
	return consumeFields(b, "cursor usable models request", func(f field) error {
		if f.number != 1 {
			return nil
		}
		if err := requireKind(f, protowire.BytesType, "cursor usable models request"); err != nil {
			return err
		}
		m.CustomModelIDs = append(m.CustomModelIDs, bytesValue(f))
		return nil
	})
}

type GetUsableModelsResponse struct {
	Models []ModelDetails
}

func AppendGetUsableModelsResponse(dst []byte, m *GetUsableModelsResponse) []byte {
	for i := range m.Models {
		dst = appendMessage(dst, 1, &m.Models[i], AppendModelDetails)
	}
	return dst
}

func ConsumeGetUsableModelsResponse(b []byte, m *GetUsableModelsResponse) error {
	return consumeFields(b, "cursor usable models response", func(f field) error {
		if f.number != 1 {
			return nil
		}
		if err := requireKind(f, protowire.BytesType, "cursor usable models response"); err != nil {
			return err
		}
		var model ModelDetails
		if err := ConsumeModelDetails(bytesValue(f), &model); err != nil {
			return err
		}
		m.Models = append(m.Models, model)
		return nil
	})
}
