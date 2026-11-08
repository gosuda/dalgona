package event

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"
)

type member struct {
	name  string
	value jsontext.Value
}

func Encode(e Event) ([]byte, error) {
	if e == nil {
		return nil, errors.New("event: cannot encode nil")
	}
	if err := e.Validate(); err != nil {
		return nil, fmt.Errorf("event: validate %s: %w", e.Type(), err)
	}
	members, err := eventMembers(e)
	if err != nil {
		return nil, err
	}
	var out bytes.Buffer
	encoder := jsontext.NewEncoder(&out)
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return nil, fmt.Errorf("event: begin object: %w", err)
	}
	if err := writeMember(encoder, "type", stringValue(e.Type())); err != nil {
		return nil, err
	}
	for _, field := range members {
		if err := writeMember(encoder, field.name, field.value); err != nil {
			return nil, err
		}
	}
	if err := encoder.WriteToken(jsontext.EndObject); err != nil {
		return nil, fmt.Errorf("event: end object: %w", err)
	}
	return bytes.TrimSuffix(out.Bytes(), []byte{'\n'}), nil
}

func writeMember(encoder *jsontext.Encoder, name string, value jsontext.Value) error {
	if err := encoder.WriteToken(jsontext.String(name)); err != nil {
		return fmt.Errorf("event: encode field %s: %w", name, err)
	}
	if err := encoder.WriteValue(value); err != nil {
		return fmt.Errorf("event: encode field %s: %w", name, err)
	}
	return nil
}

func Decode(raw []byte) (Event, error) {
	raw = bytes.Clone(raw)
	fields, err := decodeObject(raw)
	if err != nil {
		return nil, fmt.Errorf("event: decode: %w", err)
	}
	kind, err := decodeString(fields["type"])
	if err != nil {
		return nil, fmt.Errorf("event: decode type: %w", err)
	}
	reader := objectReader{fields: fields}
	decoded, err := decodeByType(kind, &reader)
	if err != nil {
		return nil, fmt.Errorf("event: decode %s: %w", kind, err)
	}
	if reader.err != nil {
		return nil, fmt.Errorf("event: decode %s: %w", kind, reader.err)
	}
	if err := decoded.Validate(); err != nil {
		return nil, fmt.Errorf("event: validate %s: %w", kind, err)
	}
	return decoded, nil
}

func decodeObject(raw []byte) (map[string]jsontext.Value, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return nil, err
	}
	if start.Kind() != jsontext.KindBeginObject {
		return nil, errors.New("event must be an object")
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
		valueStart := end - int64(len(value))
		fields[fieldName] = raw[valueStart:end]
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, err
	}
	if _, err := decoder.ReadToken(); !errors.Is(err, io.EOF) {
		return nil, errors.New("event has trailing data")
	}
	return fields, nil
}

func decodeByType(kind string, reader *objectReader) (Event, error) {
	switch kind {
	case TypeSessionCreated, TypeRunnerBound, TypeTurnStarted, TypeTurnCompleted, TypeUserMessage:
		return decodeSessionEvent(kind, reader)
	case TypeAssistantMessage, TypeToolStarted, TypeToolCompleted, TypeModelRetry, TypeApprovalRequested, TypeApprovalResolved:
		return decodeToolEvent(kind, reader)
	case TypeCompaction, TypeContextPruned, TypeChildSpawned, TypeBranch:
		return decodeStateEvent(kind, reader)
	case TypeScriptStarted, TypeScriptEffect, TypeScriptCompleted:
		return decodeScriptEvent(kind, reader)
	case TypeAdvisorNote, TypeThreadNamed, TypeThreadArchived:
		return decodeMetadataEvent(kind, reader), nil
	default:
		return nil, fmt.Errorf("unknown event type %q", kind)
	}
}

func decodeMetadataEvent(kind string, reader *objectReader) Event {
	switch kind {
	case TypeThreadNamed:
		return ThreadNamed{Name: reader.str("name")}
	case TypeThreadArchived:
		return ThreadArchived{Archived: reader.boolean("archived")}
	default:
		return AdvisorNote{
			Advisor: reader.str("advisor"), Severity: reader.str("severity"), Note: reader.str("note"),
			ReviewSeq: reader.uint64("review_seq"), Delivery: reader.str("delivery"),
		}
	}
}

func (r *objectReader) timestamp(name string) time.Time {
	text := r.str(name)
	if text == "" || r.err != nil {
		return time.Time{}
	}
	parsed, err := time.Parse(time.RFC3339Nano, text)
	r.err = err
	return parsed
}

type objectReader struct {
	fields map[string]jsontext.Value
	err    error
}

func (r *objectReader) str(name string) string {
	if r.err != nil {
		return ""
	}
	value, ok := r.fields[name]
	if !ok {
		return ""
	}
	decoded, err := decodeString(value)
	r.err = err
	return decoded
}

func (r *objectReader) number(name string) int {
	if r.err != nil {
		return 0
	}
	value, ok := r.fields[name]
	if !ok {
		return 0
	}
	decoded, err := decodeInt(value)
	r.err = err
	return decoded
}

func (r *objectReader) int64(name string) int64 {
	return int64(r.number(name))
}

func (r *objectReader) uint64(name string) uint64 {
	if r.err != nil {
		return 0
	}
	value, ok := r.fields[name]
	if !ok {
		return 0
	}
	decoded, err := decodeUint(value)
	r.err = err
	return decoded
}

func (r *objectReader) boolean(name string) bool {
	if r.err != nil {
		return false
	}
	value, ok := r.fields[name]
	if !ok {
		return false
	}
	decoded, err := decodeBool(value)
	r.err = err
	return decoded
}

func (r *objectReader) raw(name string) jsontext.Value {
	return r.fields[name]
}

func (r *objectReader) array(name string, add func(jsontext.Value) error) {
	if r.err != nil {
		return
	}
	raw, ok := r.fields[name]
	if !ok {
		return
	}
	r.err = decodeArray(raw, add)
}

func (r *objectReader) scores(name string) map[string]float64 {
	if r.err != nil {
		return nil
	}
	raw, ok := r.fields[name]
	if !ok {
		return nil
	}
	scores, err := decodeScores(raw)
	r.err = err
	return scores
}

func (r *objectReader) blob(name string) *BlobRef {
	if r.err != nil {
		return nil
	}
	raw, ok := r.fields[name]
	if !ok {
		return nil
	}
	blob, err := decodeBlob(raw)
	r.err = err
	return blob
}

func (r *objectReader) textPayload(name string) (string, *BlobRef) {
	return r.str(name), r.blob(name + "_blob")
}

func (r *objectReader) rawPayload(name string) (jsontext.Value, *BlobRef) {
	return r.raw(name), r.blob(name + "_blob")
}

func decodeString(raw jsontext.Value) (string, error) {
	if len(raw) == 0 {
		return "", errors.New("must be a string")
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", err
	}
	if token.Kind() != jsontext.KindString {
		return "", errors.New("must be a string")
	}
	return token.String(), nil
}

func decodeInt(raw jsontext.Value) (int, error) {
	token, err := scalarToken(raw)
	if err != nil {
		return 0, err
	}
	if token.Kind() != jsontext.KindNumber {
		return 0, errors.New("must be an integer")
	}
	value, err := strconv.Atoi(token.String())
	if err != nil {
		return 0, fmt.Errorf("must be an integer: %w", err)
	}
	return value, nil
}

func decodeUint(raw jsontext.Value) (uint64, error) {
	token, err := scalarToken(raw)
	if err != nil {
		return 0, err
	}
	if token.Kind() != jsontext.KindNumber {
		return 0, errors.New("must be an unsigned integer")
	}
	value, err := strconv.ParseUint(token.String(), 10, 64)
	if err != nil {
		return 0, fmt.Errorf("must be an unsigned integer: %w", err)
	}
	return value, nil
}

func decodeBool(raw jsontext.Value) (bool, error) {
	token, err := scalarToken(raw)
	if err != nil {
		return false, err
	}
	if token.Kind() != jsontext.KindTrue && token.Kind() != jsontext.KindFalse {
		return false, errors.New("must be a boolean")
	}
	return token.Bool(), nil
}

func scalarToken(raw jsontext.Value) (jsontext.Token, error) {
	if len(raw) == 0 {
		return jsontext.Token{}, errors.New("missing value")
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	return decoder.ReadToken()
}

func decodeArray(raw jsontext.Value, add func(jsontext.Value) error) error {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return err
	}
	if start.Kind() != jsontext.KindBeginArray {
		return errors.New("must be an array")
	}
	for decoder.PeekKind() != jsontext.KindEndArray {
		value, readErr := decoder.ReadValue()
		if readErr != nil {
			return readErr
		}
		end := decoder.InputOffset()
		if addErr := add(raw[end-int64(len(value)) : end]); addErr != nil {
			return addErr
		}
	}
	_, err = decoder.ReadToken()
	return err
}

func decodeScores(raw jsontext.Value) (map[string]float64, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return nil, err
	}
	scores := make(map[string]float64, len(fields))
	for name, value := range fields {
		token, err := scalarToken(value)
		if err != nil {
			return nil, err
		}
		score, err := strconv.ParseFloat(token.String(), 64)
		if err != nil {
			return nil, fmt.Errorf("score %s: %w", name, err)
		}
		scores[name] = score
	}
	return scores, nil
}

func decodeBlob(raw jsontext.Value) (*BlobRef, error) {
	fields, err := decodeObject(raw)
	if err != nil {
		return nil, err
	}
	reader := objectReader{fields: fields}
	blob := &BlobRef{Blob: reader.str("blob"), Bytes: reader.int64("bytes")}
	if reader.err != nil {
		return nil, reader.err
	}
	if err := validateBlob("blob", blob); err != nil {
		return nil, err
	}
	return blob, nil
}

func field(name string, value jsontext.Value) member {
	return member{name: name, value: value}
}

func stringValue(value string) jsontext.Value {
	if !utf8.ValidString(value) {
		value = strings.ToValidUTF8(value, "\uFFFD")
	}
	quoted, _ := jsontext.AppendQuote(nil, value)
	return jsontext.Value(quoted)
}

func intValue(value int) jsontext.Value {
	return jsontext.Value(strconv.AppendInt(nil, int64(value), 10))
}

func int64Value(value int64) jsontext.Value {
	return jsontext.Value(strconv.AppendInt(nil, value, 10))
}

func uint64Value(value uint64) jsontext.Value {
	return jsontext.Value(strconv.AppendUint(nil, value, 10))
}

func boolValue(value bool) jsontext.Value {
	return jsontext.Value(strconv.AppendBool(nil, value))
}

func floatValue(value float64) jsontext.Value {
	return jsontext.Value(strconv.AppendFloat(nil, value, 'g', -1, 64))
}

func objectValue(fields ...member) jsontext.Value {
	out := []byte{'{'}
	for i, field := range fields {
		if i > 0 {
			out = append(out, ',')
		}
		out = strconv.AppendQuote(out, field.name)
		out = append(out, ':')
		out = append(out, field.value...)
	}
	out = append(out, '}')
	return jsontext.Value(out)
}

func arrayValue[T any](items []T, encode func(T) jsontext.Value) jsontext.Value {
	out := []byte{'['}
	for i, item := range items {
		if i > 0 {
			out = append(out, ',')
		}
		out = append(out, encode(item)...)
	}
	out = append(out, ']')
	return jsontext.Value(out)
}

func blobValue(blob *BlobRef) jsontext.Value {
	if blob == nil {
		return nil
	}
	return objectValue(field("blob", stringValue(blob.Blob)), field("bytes", int64Value(blob.Bytes)))
}

func optionalString(fields []member, name, value string) []member {
	if value == "" {
		return fields
	}
	return append(fields, field(name, stringValue(value)))
}

func optionalRaw(fields []member, name string, value jsontext.Value) []member {
	if len(value) == 0 {
		return fields
	}
	return append(fields, field(name, value))
}
