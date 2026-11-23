package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	"errors"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

type settingEditParams struct {
	keyPath       string
	value         any
	mergeStrategy string
}

type batchWriteParams struct {
	edits           []settingEditParams
	filePath        string
	expectedVersion string
}

var wireSettingNames = map[string]string{
	"status_hide":         "statusHide",
	"edit_review.enabled": "editReview",
}

func (s *server) writeEdits(expectedVersion string, edits ...config.SettingEdit) (string, error) {
	version, err := s.host.WriteSettingsIf(expectedVersion, edits...)
	switch {
	case errors.Is(err, config.ErrSettingsChanged):
		return "", invalidRequest(err.Error())
	case err != nil:
		return "", invalidParams(err.Error())
	}
	return version, nil
}

func (s *server) configBatchWrite(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	request, err := decodeBatchWrite(params)
	if err != nil {
		return nil, err
	}
	edits, err := request.settingEdits()
	if err != nil {
		return nil, err
	}
	if len(edits) == 0 {
		return nil, invalidParams("edits must not be empty")
	}
	cfg := s.host.Config()
	path := cfg.SettingsPath()
	if request.filePath != "" && request.filePath != path {
		return nil, invalidParams("filePath must be " + path)
	}
	version, err := s.writeEdits(request.expectedVersion, edits...)
	if err != nil {
		return nil, err
	}
	return rpc.Marshal(rpc.ConfigWriteResponse{FilePath: path, Status: "ok", Version: version})
}

func decodeBatchWrite(params jsontext.Value) (batchWriteParams, error) {
	var request batchWriteParams
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "edits":
			request.edits, err = readEditList(d)
		case "filePath":
			request.filePath, err = rpc.ReadString(d)
		case "expectedVersion":
			request.expectedVersion, err = rpc.ReadString(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
	return request, err
}

func readEditList(d *jsontext.Decoder) ([]settingEditParams, error) {
	var edits []settingEditParams
	err := rpc.ReadArray(d, func() error {
		edit, err := readEdit(d)
		edits = append(edits, edit)
		return err
	})
	return edits, err
}

func readEdit(d *jsontext.Decoder) (settingEditParams, error) {
	var edit settingEditParams
	err := rpc.ReadMembers(d, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "keyPath":
			edit.keyPath, err = rpc.ReadString(d)
		case "value":
			edit.value, err = readPlain(d)
		case "mergeStrategy":
			edit.mergeStrategy, err = rpc.ReadString(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
	return edit, err
}

func (r batchWriteParams) settingEdits() ([]config.SettingEdit, error) {
	edits := make([]config.SettingEdit, 0, len(r.edits))
	for _, edit := range r.edits {
		if edit.mergeStrategy != "replace" && edit.mergeStrategy != "upsert" {
			return nil, invalidParams("unsupported mergeStrategy " + edit.mergeStrategy)
		}
		key, value, err := settingKey(edit.keyPath, edit.value)
		if err != nil {
			return nil, err
		}
		edits = append(edits, config.SettingEdit{Key: key, Value: value})
	}
	return edits, nil
}

func (s *server) settingsRead(context.Context, jsontext.Value) (jsontext.Value, error) {
	return rpc.Marshal(settingsReadResponse(s.host.Config()))
}

func settingsReadResponse(cfg config.Config) rpc.SettingsReadResponse {
	response := rpc.SettingsReadResponse{Settings: make([]rpc.Setting, 0, len(config.InteractiveSettings))}
	for _, spec := range config.InteractiveSettings {
		if cfg.Product == config.ProductDal && spec.Key == "edit_review.enabled" {
			continue
		}
		response.Settings = append(response.Settings, rpc.Setting{
			Key: wireName(spec.Key), Value: cfg.Setting(spec.Key), Default: spec.Default, Options: spec.Options,
		})
	}
	return response
}

func wireName(key string) string {
	if name, ok := wireSettingNames[key]; ok {
		return name
	}
	return key
}
