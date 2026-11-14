package runner

import (
	"bufio"
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"io"
	"io/fs"
	"strings"
	"sync"
)

const maxRPCLine = 16 << 20

const readPageSize = 1 << 20

const (
	rpcParseError     = -32700
	rpcInvalidParams  = -32602
	rpcMethodNotFound = -32601
	rpcInternal       = -32000
	rpcProcessMissing = -32001
	rpcFileMissing    = -32002
)

var ErrSandboxDowngrade = errors.New("runner: sandbox downgrade refused")

var (
	errParse          = errors.New("parse error")
	errInvalidParams  = rpcStatusError{code: rpcInvalidParams, msg: "invalid params"}
	errMethodNotFound = rpcStatusError{code: rpcMethodNotFound, msg: "method not found"}
	errLineTooLong    = errors.New("runner: rpc line exceeds 16MiB")
	errRemoteClosed   = errors.New("runner: remote closed")
)

type rpcStatusError struct {
	code int
	msg  string
}

func (e rpcStatusError) Error() string { return e.msg }

type rpcMessage struct {
	hasID   bool
	id      jsontext.Value
	method  string
	params  jsontext.Value
	result  jsontext.Value
	hasErr  bool
	errCode int
	errMsg  string
}

type rpcWriter struct {
	w  io.Writer
	mu *sync.Mutex
}

func readLimitedLine(r *bufio.Reader, limit int) ([]byte, error) {
	var out []byte
	for {
		frag, err := r.ReadSlice('\n')
		if len(out)+len(frag) > limit+1 {
			return nil, errLineTooLong
		}
		out = append(out, frag...)
		if err == nil {
			return trimNL(out), nil
		}
		if !errors.Is(err, bufio.ErrBufferFull) {
			return nil, lineEndError(out, err)
		}
	}
}

func lineEndError(out []byte, err error) error {
	if errors.Is(err, io.EOF) && len(out) == 0 {
		return io.EOF
	}
	if errors.Is(err, io.EOF) {
		return io.ErrUnexpectedEOF
	}
	return err
}

func trimNL(line []byte) []byte {
	line = bytes.TrimSuffix(line, []byte("\n"))
	return bytes.TrimSuffix(line, []byte("\r"))
}

func parseRPC(line []byte) (rpcMessage, error) {
	dec := jsontext.NewDecoder(bytes.NewReader(line))
	tok, err := dec.ReadToken()
	if err != nil || tok.Kind() != jsontext.KindBeginObject {
		return rpcMessage{}, errParse
	}
	var msg rpcMessage
	for dec.PeekKind() != jsontext.KindEndObject {
		if err := readRPCField(dec, &msg); err != nil {
			return rpcMessage{}, err
		}
	}
	if _, err := dec.ReadToken(); err != nil {
		return rpcMessage{}, errParse
	}
	if _, err := dec.ReadValue(); !errors.Is(err, io.EOF) {
		return rpcMessage{}, errParse
	}
	return msg, nil
}

func readRPCField(dec *jsontext.Decoder, msg *rpcMessage) error {
	key, err := dec.ReadToken()
	if err != nil || key.Kind() != jsontext.KindString {
		return errParse
	}
	return applyRPCField(dec, msg, key.String())
}

func applyRPCField(dec *jsontext.Decoder, msg *rpcMessage, key string) error {
	switch key {
	case "id":
		return readRPCID(dec, msg)
	case "method":
		return readRPCMethod(dec, msg)
	case "params":
		return readRPCValue(dec, &msg.params)
	case "result":
		return readRPCValue(dec, &msg.result)
	case "error":
		return readRPCError(dec, msg)
	default:
		return skipValue(dec)
	}
}

func readRPCID(dec *jsontext.Decoder, msg *rpcMessage) error {
	val, err := dec.ReadValue()
	if err != nil {
		return errParse
	}
	msg.hasID = true
	msg.id = val.Clone()
	return nil
}

func readRPCMethod(dec *jsontext.Decoder, msg *rpcMessage) error {
	tok, err := dec.ReadToken()
	if err != nil || tok.Kind() != jsontext.KindString {
		return errParse
	}
	msg.method = strings.Clone(tok.String())
	return nil
}

func readRPCValue(dec *jsontext.Decoder, dst *jsontext.Value) error {
	val, err := dec.ReadValue()
	if err != nil {
		return errParse
	}
	*dst = val.Clone()
	return nil
}

func readRPCError(dec *jsontext.Decoder, msg *rpcMessage) error {
	tok, err := dec.ReadToken()
	if err != nil || tok.Kind() != jsontext.KindBeginObject {
		return errParse
	}
	msg.hasErr = true
	for dec.PeekKind() != jsontext.KindEndObject {
		key, err := dec.ReadToken()
		if err != nil || key.Kind() != jsontext.KindString {
			return errParse
		}
		if err := applyRPCErrorField(dec, msg, key.String()); err != nil {
			return err
		}
	}
	if _, err := dec.ReadToken(); err != nil {
		return errParse
	}
	return nil
}

func applyRPCErrorField(dec *jsontext.Decoder, msg *rpcMessage, key string) error {
	switch key {
	case "code":
		code, err := readIntToken(dec)
		if err != nil {
			return err
		}
		msg.errCode = int(code)
		return nil
	case "message":
		text, err := readStringToken(dec)
		if err != nil {
			return err
		}
		msg.errMsg = text
		return nil
	default:
		return skipValue(dec)
	}
}

func readIntToken(dec *jsontext.Decoder) (int64, error) {
	val, err := dec.ReadToken()
	if err != nil || val.Kind() != jsontext.KindNumber {
		return 0, errParse
	}
	code, err := val.Int()
	if err != nil {
		return 0, errParse
	}
	return code, nil
}

func readStringToken(dec *jsontext.Decoder) (string, error) {
	val, err := dec.ReadToken()
	if err != nil || val.Kind() != jsontext.KindString {
		return "", errParse
	}
	return strings.Clone(val.String()), nil
}

func skipValue(dec *jsontext.Decoder) error {
	if _, err := dec.ReadValue(); err != nil {
		return errParse
	}
	return nil
}

func writeRPCResult(writer rpcWriter, id, result jsontext.Value) error {
	payload, err := encodeEnvelope(id, "result", resultOrEmpty(result))
	if err != nil {
		return err
	}
	return writePayload(writer, payload)
}

func writeRPCError(writer rpcWriter, id jsontext.Value, code int, msg string) error {
	body, err := encodeErrorBody(code, msg)
	if err != nil {
		return err
	}
	payload, err := encodeEnvelope(id, "error", body)
	if err != nil {
		return err
	}
	return writePayload(writer, payload)
}

func writePayload(writer rpcWriter, payload []byte) error {
	if writer.mu != nil {
		writer.mu.Lock()
		defer writer.mu.Unlock()
	}
	return writeFull(writer.w, payload)
}

func writeFull(w io.Writer, payload []byte) error {
	for len(payload) > 0 {
		n, err := w.Write(payload)
		if err != nil {
			return err
		}
		payload = payload[n:]
	}
	return nil
}

func encodeEnvelope(id jsontext.Value, field string, body jsontext.Value) ([]byte, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeRaw(enc, "id", idOrNull(id)); err != nil {
		return nil, err
	}
	if err := writeRaw(enc, field, body); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	buf.WriteByte('\n')
	return buf.Bytes(), nil
}

func encodeErrorBody(code int, msg string) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeInt(enc, "code", int64(code)); err != nil {
		return nil, err
	}
	if err := writeString(enc, "message", msg); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func encodeRequest(id int64, method string, params jsontext.Value) ([]byte, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return nil, err
	}
	if err := writeInt(enc, "id", id); err != nil {
		return nil, err
	}
	if err := writeString(enc, "method", method); err != nil {
		return nil, err
	}
	if err := writeRaw(enc, "params", resultOrEmpty(params)); err != nil {
		return nil, err
	}
	if err := enc.WriteToken(jsontext.EndObject); err != nil {
		return nil, err
	}
	buf.WriteByte('\n')
	return buf.Bytes(), nil
}

func paramsFields(raw jsontext.Value) (map[string]jsontext.Value, error) {
	if len(raw) == 0 || raw.Kind() == jsontext.KindNull {
		return map[string]jsontext.Value{}, nil
	}
	return objectFields(raw)
}

func objectFields(raw jsontext.Value) (map[string]jsontext.Value, error) {
	dec := jsontext.NewDecoder(bytes.NewReader(raw))
	tok, err := dec.ReadToken()
	if err != nil || tok.Kind() != jsontext.KindBeginObject {
		return nil, errInvalidParams
	}
	fields := make(map[string]jsontext.Value)
	for dec.PeekKind() != jsontext.KindEndObject {
		if err := readObjectField(dec, fields); err != nil {
			return nil, err
		}
	}
	if _, err := dec.ReadToken(); err != nil {
		return nil, errInvalidParams
	}
	return fields, nil
}

func readObjectField(dec *jsontext.Decoder, fields map[string]jsontext.Value) error {
	key, err := dec.ReadToken()
	if err != nil || key.Kind() != jsontext.KindString {
		return errInvalidParams
	}
	name := strings.Clone(key.String())
	val, err := dec.ReadValue()
	if err != nil {
		return errInvalidParams
	}
	fields[name] = val.Clone()
	return nil
}

func valueStringMap(raw jsontext.Value) (map[string]string, error) {
	fields, err := objectFields(raw)
	if err != nil {
		return nil, err
	}
	out := make(map[string]string, len(fields))
	for key, val := range fields {
		text, err := valueString(val)
		if err != nil {
			return nil, err
		}
		out[key] = text
	}
	return out, nil
}

func fieldString(fields map[string]jsontext.Value, key string) (string, error) {
	raw, ok := fields[key]
	if !ok {
		return "", errInvalidParams
	}
	return valueString(raw)
}

func optionalString(fields map[string]jsontext.Value, key string) (string, error) {
	raw, ok := fields[key]
	if !ok || raw.Kind() == jsontext.KindNull {
		return "", nil
	}
	return valueString(raw)
}

func fieldStrings(fields map[string]jsontext.Value, key string) ([]string, error) {
	raw, ok := fields[key]
	if !ok {
		return nil, errInvalidParams
	}
	return valueStrings(raw)
}

func optionalStrings(fields map[string]jsontext.Value, key string) ([]string, error) {
	raw, ok := fields[key]
	if !ok || raw.Kind() == jsontext.KindNull {
		return nil, nil
	}
	return valueStrings(raw)
}

func fieldInt(fields map[string]jsontext.Value, key string) (int64, error) {
	raw, ok := fields[key]
	if !ok {
		return 0, errInvalidParams
	}
	return valueInt(raw)
}

func fieldMode(fields map[string]jsontext.Value, key string) (fs.FileMode, error) {
	mode, err := fieldInt(fields, key)
	if err != nil {
		return 0, err
	}
	if mode < 0 || mode > 1<<32-1 {
		return 0, errInvalidParams
	}
	return fs.FileMode(uint32(mode)), nil
}

func optionalInt(fields map[string]jsontext.Value, key string) (int64, error) {
	raw, ok := fields[key]
	if !ok || raw.Kind() == jsontext.KindNull {
		return 0, nil
	}
	return valueInt(raw)
}

func fieldBool(fields map[string]jsontext.Value, key string) (bool, error) {
	raw, ok := fields[key]
	if !ok {
		return false, errInvalidParams
	}
	return valueBool(raw)
}

func valueString(raw jsontext.Value) (string, error) {
	tok, err := oneToken(raw)
	if err != nil || tok.Kind() != jsontext.KindString {
		return "", errInvalidParams
	}
	return tok.String(), nil
}

func valueInt(raw jsontext.Value) (int64, error) {
	tok, err := oneToken(raw)
	if err != nil || tok.Kind() != jsontext.KindNumber {
		return 0, errInvalidParams
	}
	return tok.Int()
}

func valueBool(raw jsontext.Value) (bool, error) {
	tok, err := oneToken(raw)
	if err != nil {
		return false, errInvalidParams
	}
	kind := tok.Kind()
	if kind != jsontext.KindTrue && kind != jsontext.KindFalse {
		return false, errInvalidParams
	}
	return kind == jsontext.KindTrue, nil
}

func valueStrings(raw jsontext.Value) ([]string, error) {
	dec := jsontext.NewDecoder(bytes.NewReader(raw))
	tok, err := dec.ReadToken()
	if err != nil || tok.Kind() != jsontext.KindBeginArray {
		return nil, errInvalidParams
	}
	var out []string
	for dec.PeekKind() != jsontext.KindEndArray {
		text, err := readStringValue(dec)
		if err != nil {
			return nil, err
		}
		out = append(out, text)
	}
	if _, err := dec.ReadToken(); err != nil {
		return nil, errInvalidParams
	}
	return out, nil
}

func readStringValue(dec *jsontext.Decoder) (string, error) {
	item, err := dec.ReadValue()
	if err != nil {
		return "", errInvalidParams
	}
	return valueString(item)
}

func oneToken(raw jsontext.Value) (jsontext.Token, error) {
	dec := jsontext.NewDecoder(bytes.NewReader(raw))
	return dec.ReadToken()
}

func writeString(enc *jsontext.Encoder, key, value string) error {
	if err := enc.WriteToken(jsontext.String(key)); err != nil {
		return err
	}
	return enc.WriteToken(jsontext.String(value))
}

func writeInt(enc *jsontext.Encoder, key string, value int64) error {
	if err := enc.WriteToken(jsontext.String(key)); err != nil {
		return err
	}
	return enc.WriteToken(jsontext.Int(value))
}

func writeBool(enc *jsontext.Encoder, key string, value bool) error {
	if err := enc.WriteToken(jsontext.String(key)); err != nil {
		return err
	}
	return enc.WriteToken(jsontext.Bool(value))
}

func writeRaw(enc *jsontext.Encoder, key string, value jsontext.Value) error {
	if err := enc.WriteToken(jsontext.String(key)); err != nil {
		return err
	}
	return enc.WriteValue(value)
}

func idOrNull(id jsontext.Value) jsontext.Value {
	if len(id) == 0 {
		return jsontext.Value("null")
	}
	return id
}

func resultOrEmpty(result jsontext.Value) jsontext.Value {
	if len(result) == 0 {
		return emptyObject()
	}
	return result
}

func emptyObject() jsontext.Value { return jsontext.Value("{}") }
