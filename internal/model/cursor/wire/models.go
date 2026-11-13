package wire

import (
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"google.golang.org/protobuf/encoding/protowire"
)

type ModelParameter struct {
	IDView    []byte
	ValueView []byte
}

func AppendModelParameter(dst []byte, m *ModelParameter) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.IDView)
	return connectwire.AppendRaw(dst, 2, m.ValueView)
}

func ConsumeModelParameter(b []byte, m *ModelParameter) error {
	return connectwire.ConsumeFields(b, "cursor model parameter", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "cursor model parameter", &m.IDView)
		case 2:
			return connectwire.SetBytes(f, "cursor model parameter", &m.ValueView)
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
	dst = connectwire.AppendRaw(dst, 1, m.ModelIDView)
	dst = connectwire.AppendBool(dst, 2, m.MaxMode)
	for i := range m.Parameters {
		dst = connectwire.AppendMessage(dst, 3, &m.Parameters[i], AppendModelParameter)
	}
	if m.CredentialNumber != 0 {
		dst = connectwire.AppendVariant(dst, protowire.Number(m.CredentialNumber), m.CredentialsView)
	}
	return dst
}

func ConsumeRequestedModel(b []byte, m *RequestedModel) error {
	return connectwire.ConsumeFields(b, "cursor requested model", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "cursor requested model", &m.ModelIDView)
		case 2:
			return connectwire.SetBool(f, "cursor requested model", &m.MaxMode)
		case 3:
			if err := connectwire.RequireKind(f, protowire.BytesType, "cursor requested model"); err != nil {
				return err
			}
			var p ModelParameter
			if err := ConsumeModelParameter(connectwire.BytesValue(f), &p); err != nil {
				return err
			}
			m.Parameters = append(m.Parameters, p)
		case 4, 5, 6:
			m.CredentialNumber = f.Number
			return connectwire.SetBytes(f, "cursor requested model", &m.CredentialsView)
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
	dst = connectwire.AppendRaw(dst, 1, m.ModelIDView)
	dst = connectwire.AppendRaw(dst, 2, m.ThinkingDetailsView)
	dst = connectwire.AppendRaw(dst, 3, m.DisplayModelIDView)
	dst = connectwire.AppendRaw(dst, 4, m.DisplayNameView)
	dst = connectwire.AppendRaw(dst, 5, m.DisplayShortView)
	dst = connectwire.AppendRepeatedBytes(dst, 6, m.Aliases)
	dst = connectwire.AppendBool(dst, 7, m.MaxMode)
	if m.CredentialNumber != 0 {
		dst = connectwire.AppendVariant(dst, protowire.Number(m.CredentialNumber), m.CredentialsView)
	}
	return dst
}

func ConsumeModelDetails(b []byte, m *ModelDetails) error {
	return connectwire.ConsumeFields(b, "cursor model details", func(f connectwire.Field) error {
		if f.Number <= 5 {
			return consumeModelIdentity(f, m)
		}
		return consumeModelOptions(f, m)
	})
}

func consumeModelIdentity(f connectwire.Field, m *ModelDetails) error {
	switch f.Number {
	case 1:
		return connectwire.SetBytes(f, "cursor model details", &m.ModelIDView)
	case 2:
		return connectwire.SetBytes(f, "cursor model details", &m.ThinkingDetailsView)
	case 3:
		return connectwire.SetBytes(f, "cursor model details", &m.DisplayModelIDView)
	case 4:
		return connectwire.SetBytes(f, "cursor model details", &m.DisplayNameView)
	case 5:
		return connectwire.SetBytes(f, "cursor model details", &m.DisplayShortView)
	default:
		return nil
	}
}

func consumeModelOptions(f connectwire.Field, m *ModelDetails) error {
	switch f.Number {
	case 6:
		if err := connectwire.RequireKind(f, protowire.BytesType, "cursor model details"); err != nil {
			return err
		}
		m.Aliases = append(m.Aliases, connectwire.BytesValue(f))
	case 7:
		return connectwire.SetBool(f, "cursor model details", &m.MaxMode)
	case 8, 9, 10:
		m.CredentialNumber = f.Number
		return connectwire.SetBytes(f, "cursor model details", &m.CredentialsView)
	}
	return nil
}

type GetUsableModelsRequest struct {
	CustomModelIDs [][]byte
}

func AppendGetUsableModelsRequest(dst []byte, m *GetUsableModelsRequest) []byte {
	return connectwire.AppendRepeatedBytes(dst, 1, m.CustomModelIDs)
}

func ConsumeGetUsableModelsRequest(b []byte, m *GetUsableModelsRequest) error {
	return connectwire.ConsumeFields(b, "cursor usable models request", func(f connectwire.Field) error {
		if f.Number != 1 {
			return nil
		}
		if err := connectwire.RequireKind(f, protowire.BytesType, "cursor usable models request"); err != nil {
			return err
		}
		m.CustomModelIDs = append(m.CustomModelIDs, connectwire.BytesValue(f))
		return nil
	})
}

type GetUsableModelsResponse struct {
	Models []ModelDetails
}

func AppendGetUsableModelsResponse(dst []byte, m *GetUsableModelsResponse) []byte {
	for i := range m.Models {
		dst = connectwire.AppendMessage(dst, 1, &m.Models[i], AppendModelDetails)
	}
	return dst
}

func ConsumeGetUsableModelsResponse(b []byte, m *GetUsableModelsResponse) error {
	return connectwire.ConsumeFields(b, "cursor usable models response", func(f connectwire.Field) error {
		if f.Number != 1 {
			return nil
		}
		if err := connectwire.RequireKind(f, protowire.BytesType, "cursor usable models response"); err != nil {
			return err
		}
		var model ModelDetails
		if err := ConsumeModelDetails(connectwire.BytesValue(f), &model); err != nil {
			return err
		}
		m.Models = append(m.Models, model)
		return nil
	})
}
