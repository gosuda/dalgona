package mcp

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
)

type messageWriter struct {
	encoder *jsontext.Encoder
	err     error
}

func (w *messageWriter) token(value jsontext.Token) {
	if w.err == nil {
		w.err = w.encoder.WriteToken(value)
	}
}

func (w *messageWriter) value(value jsontext.Value) {
	if w.err == nil {
		w.err = w.encoder.WriteValue(value)
	}
}

func (w *messageWriter) stringMember(name, value string) {
	w.token(jsontext.String(name))
	w.token(jsontext.String(value))
}

func (w *messageWriter) objectMember(name string) {
	w.token(jsontext.String(name))
	w.token(jsontext.BeginObject)
	w.token(jsontext.EndObject)
}

func encodeMessage(id *uint64, method string, params jsontext.Value) ([]byte, error) {
	if len(params) > 0 && !params.IsValid() {
		return nil, errors.New("mcp: request params must be valid JSON")
	}
	var out bytes.Buffer
	out.Grow(len(params) + len(method) + 64)
	writer := messageWriter{encoder: jsontext.NewEncoder(&out)}
	writer.token(jsontext.BeginObject)
	writer.stringMember("jsonrpc", "2.0")
	if id != nil {
		writer.token(jsontext.String("id"))
		writer.token(jsontext.Uint(*id))
	}
	writer.stringMember("method", method)
	if len(params) > 0 {
		writer.token(jsontext.String("params"))
		writer.value(params)
	}
	writer.token(jsontext.EndObject)
	if writer.err != nil {
		return nil, writer.err
	}
	return bytes.TrimSuffix(out.Bytes(), []byte{'\n'}), nil
}

func encodeListParams(cursor string) (jsontext.Value, error) {
	var out bytes.Buffer
	writer := messageWriter{encoder: jsontext.NewEncoder(&out)}
	writer.token(jsontext.BeginObject)
	if cursor != "" {
		writer.stringMember("cursor", cursor)
	}
	writer.token(jsontext.EndObject)
	if writer.err != nil {
		return nil, writer.err
	}
	return bytes.TrimSuffix(out.Bytes(), []byte{'\n'}), nil
}

func encodeCallParams(name string, args jsontext.Value) (jsontext.Value, error) {
	if len(args) == 0 {
		args = jsontext.Value(`{}`)
	}
	if !args.IsValid() || args.Kind() != jsontext.KindBeginObject {
		return nil, errors.New("mcp: tool arguments must be a JSON object")
	}
	var out bytes.Buffer
	out.Grow(len(args) + len(name) + 32)
	writer := messageWriter{encoder: jsontext.NewEncoder(&out)}
	writer.token(jsontext.BeginObject)
	writer.stringMember("name", name)
	writer.token(jsontext.String("arguments"))
	writer.value(args)
	writer.token(jsontext.EndObject)
	if writer.err != nil {
		return nil, writer.err
	}
	return bytes.TrimSuffix(out.Bytes(), []byte{'\n'}), nil
}

func encodeInitializeParams() (jsontext.Value, error) {
	var out bytes.Buffer
	writer := messageWriter{encoder: jsontext.NewEncoder(&out)}
	writer.token(jsontext.BeginObject)
	writer.stringMember("protocolVersion", protocolVersion)
	writer.objectMember("capabilities")
	writer.token(jsontext.String("clientInfo"))
	writer.token(jsontext.BeginObject)
	writer.stringMember("name", clientName)
	writer.stringMember("version", clientVersion())
	writer.token(jsontext.EndObject)
	writer.token(jsontext.EndObject)
	if writer.err != nil {
		return nil, writer.err
	}
	return bytes.TrimSuffix(out.Bytes(), []byte{'\n'}), nil
}

func decodeObject(raw []byte) (map[string]jsontext.Value, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return nil, err
	}
	if start.Kind() != jsontext.KindBeginObject {
		return nil, errors.New("expected a JSON object")
	}
	fields := make(map[string]jsontext.Value)
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := decoder.ReadToken()
		if err != nil {
			return nil, err
		}
		fieldName := name.String()
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, err
		}
		end := decoder.InputOffset()
		fields[fieldName] = raw[end-int64(len(value)) : end]
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, err
	}
	if err := requireJSONEOF(decoder); err != nil {
		return nil, err
	}
	return fields, nil
}

func decodeArray(raw []byte) ([]jsontext.Value, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return nil, err
	}
	if start.Kind() != jsontext.KindBeginArray {
		return nil, errors.New("expected a JSON array")
	}
	var values []jsontext.Value
	for decoder.PeekKind() != jsontext.KindEndArray {
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, err
		}
		end := decoder.InputOffset()
		values = append(values, raw[end-int64(len(value)):end])
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, err
	}
	if err := requireJSONEOF(decoder); err != nil {
		return nil, err
	}
	return values, nil
}

func decodeString(raw []byte) (string, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", err
	}
	if token.Kind() != jsontext.KindString {
		return "", errors.New("expected a JSON string")
	}
	value := token.String()
	if err := requireJSONEOF(decoder); err != nil {
		return "", err
	}
	return value, nil
}

func decodeBool(raw []byte) (bool, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	token, err := decoder.ReadToken()
	if err != nil {
		return false, err
	}
	if token.Kind() != jsontext.KindTrue && token.Kind() != jsontext.KindFalse {
		return false, errors.New("expected a JSON boolean")
	}
	value := token.Bool()
	if err := requireJSONEOF(decoder); err != nil {
		return false, err
	}
	return value, nil
}

func decodeUint(raw []byte) (uint64, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	token, err := decoder.ReadToken()
	if err != nil {
		return 0, err
	}
	if token.Kind() != jsontext.KindNumber {
		return 0, errors.New("expected a JSON number")
	}
	value, err := token.Uint()
	if err != nil {
		return 0, err
	}
	if err := requireJSONEOF(decoder); err != nil {
		return 0, err
	}
	return value, nil
}

func requireJSONEOF(decoder *jsontext.Decoder) error {
	_, err := decoder.ReadToken()
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err != nil {
		return err
	}
	return errors.New("unexpected trailing JSON")
}

type rpcError struct {
	Code    int64
	Message string
}

func (e *rpcError) Error() string {
	return fmt.Sprintf("server error %d: %s", e.Code, e.Message)
}

func matchResponse(raw []byte, expectedID uint64) (bool, jsontext.Value, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return false, nil, err
	}
	if _, hasMethod := fields["method"]; hasMethod {
		return false, nil, nil
	}
	if versionErr := checkJSONRPCVersion(fields); versionErr != nil {
		return false, nil, versionErr
	}
	idValue, hasID := fields["id"]
	if !hasID {
		return false, nil, nil
	}
	id, err := decodeUint(idValue)
	if err != nil {
		return false, nil, fmt.Errorf("decode response id: %w", err)
	}
	if id != expectedID {
		return false, nil, nil
	}
	payload, err := responsePayload(fields)
	return true, payload, err
}

func checkJSONRPCVersion(fields map[string]jsontext.Value) error {
	version, hasVersion := fields["jsonrpc"]
	if !hasVersion {
		return errors.New("missing JSON-RPC version")
	}
	value, err := decodeString(version)
	if err != nil {
		return fmt.Errorf("decode JSON-RPC version: %w", err)
	}
	if value != "2.0" {
		return errors.New("invalid JSON-RPC version")
	}
	return nil
}

func responsePayload(fields map[string]jsontext.Value) (jsontext.Value, error) {
	result, hasResult := fields["result"]
	rawError, hasError := fields["error"]
	if hasResult && hasError {
		return nil, errors.New("response has both result and error")
	}
	if !hasError {
		if !hasResult {
			return nil, errors.New("response has neither result nor error")
		}
		return result, nil
	}
	rpcErr, err := decodeRPCError(rawError)
	if err != nil {
		return nil, err
	}
	return nil, rpcErr
}

func decodeRPCError(raw []byte) (*rpcError, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return nil, err
	}
	code, err := decodeInt(fields["code"])
	if err != nil {
		return nil, err
	}
	message, err := decodeString(fields["message"])
	if err != nil {
		return nil, err
	}
	return &rpcError{Code: code, Message: message}, nil
}

func decodeInt(raw []byte) (int64, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	token, err := decoder.ReadToken()
	if err != nil {
		return 0, err
	}
	if token.Kind() != jsontext.KindNumber {
		return 0, errors.New("expected a JSON number")
	}
	value, err := token.Int()
	if err != nil {
		return 0, err
	}
	if err := requireJSONEOF(decoder); err != nil {
		return 0, err
	}
	return value, nil
}
