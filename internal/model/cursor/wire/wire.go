package wire

import (
	"bytes"
	"encoding/binary"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"math"

	"google.golang.org/protobuf/encoding/protowire"
)

const (
	Compressed byte = 1
	EndStream  byte = 2
)

type Frame struct {
	Flags   byte
	Payload []byte
}

func AppendFrame(dst []byte, flags byte, payload []byte) []byte {
	if uint64(len(payload)) > math.MaxUint32 {
		panic("connect frame payload exceeds uint32")
	}
	n := uint64(len(payload))
	dst = append(dst, flags, byte((n>>24)&0xff), byte((n>>16)&0xff), byte((n>>8)&0xff), byte(n&0xff))
	return append(dst, payload...)
}

func ReadFrame(r io.Reader, buf []byte, limit uint32) (Frame, error) {
	var h [5]byte
	if _, err := io.ReadFull(r, h[:]); err != nil {
		return Frame{}, fmt.Errorf("connect frame header: %w", err)
	}
	n := binary.BigEndian.Uint32(h[1:])
	if n > limit {
		return Frame{}, fmt.Errorf("connect frame length %d exceeds %d", n, limit)
	}
	if uint64(cap(buf)) < uint64(n) {
		buf = make([]byte, n)
	}
	p := buf[:n]
	if _, err := io.ReadFull(r, p); err != nil {
		return Frame{}, fmt.Errorf("connect frame payload: %w", err)
	}
	return Frame{Flags: h[0], Payload: p}, nil
}

type ConnectError struct {
	Code    string
	Message string
}

func (e *ConnectError) Error() string {
	if e.Code == "" {
		return e.Message
	}
	if e.Message == "" {
		return e.Code
	}
	return e.Code + ": " + e.Message
}

func TrailerError(payload []byte) error {
	if len(bytes.TrimSpace(payload)) == 0 {
		return nil
	}
	out, err := readTrailer(jsontext.NewDecoder(bytes.NewReader(payload)))
	if err != nil {
		return err
	}
	if out.Code == "" && out.Message == "" {
		return nil
	}
	return &out
}

func readTrailer(d *jsontext.Decoder) (ConnectError, error) {
	var out ConnectError
	t, err := d.ReadToken()
	if err != nil {
		return out, fmt.Errorf("connect trailer: %w", err)
	}
	if t.Kind() != '{' {
		return out, errors.New("connect trailer is not an object")
	}
	if err = readTrailerFields(d, &out); err != nil {
		return out, err
	}
	return out, requireTrailerEnd(d)
}

func requireTrailerEnd(d *jsontext.Decoder) error {
	_, err := d.ReadToken()
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("connect trailer end: %w", err)
	}
	return errors.New("connect trailer has trailing JSON")
}

func readTrailerFields(d *jsontext.Decoder, out *ConnectError) error {
	for d.PeekKind() != '}' {
		key, err := d.ReadToken()
		if err != nil {
			return fmt.Errorf("connect trailer key: %w", err)
		}
		if err := readTrailerValue(d, key.String(), out); err != nil {
			return err
		}
	}
	if _, err := d.ReadToken(); err != nil {
		return fmt.Errorf("connect trailer end: %w", err)
	}
	return nil
}

func readTrailerValue(d *jsontext.Decoder, key string, out *ConnectError) error {
	if key == "error" && d.PeekKind() == '{' {
		if _, err := d.ReadToken(); err != nil {
			return fmt.Errorf("connect trailer error: %w", err)
		}
		return readTrailerErrorObject(d, out)
	}
	if d.PeekKind() == '"' && (key == "code" || key == "message") {
		return readTrailerString(d, key, out)
	}
	if _, err := d.ReadValue(); err != nil {
		return fmt.Errorf("connect trailer value: %w", err)
	}
	return nil
}

func readTrailerErrorObject(d *jsontext.Decoder, out *ConnectError) error {
	for d.PeekKind() != '}' {
		key, err := d.ReadToken()
		if err != nil {
			return fmt.Errorf("connect trailer error key: %w", err)
		}
		name := key.String()
		if name != "code" && name != "message" {
			if _, err := d.ReadValue(); err != nil {
				return fmt.Errorf("connect trailer error value: %w", err)
			}
			continue
		}
		if d.PeekKind() != '"' {
			return fmt.Errorf("connect trailer %s is not a string", name)
		}
		if err := readTrailerString(d, name, out); err != nil {
			return err
		}
	}
	if _, err := d.ReadToken(); err != nil {
		return fmt.Errorf("connect trailer error end: %w", err)
	}
	return nil
}

func readTrailerString(d *jsontext.Decoder, key string, out *ConnectError) error {
	t, err := d.ReadToken()
	if err != nil {
		return fmt.Errorf("connect trailer %s: %w", key, err)
	}
	if key == "code" {
		out.Code = t.String()
	} else {
		out.Message = t.String()
	}
	return nil
}

type field struct {
	number int32
	kind   protowire.Type
	raw    []byte
	data   []byte
}

func consumeFields(b []byte, name string, fn func(field) error) error {
	for len(b) > 0 {
		f, rest, err := consumeField(b, name)
		if err != nil {
			return err
		}
		if fn != nil {
			if err := fn(f); err != nil {
				return err
			}
		}
		b = rest
	}
	return nil
}

func consumeField(b []byte, name string) (field, []byte, error) {
	n, typ, k := protowire.ConsumeTag(b)
	if k < 0 {
		return field{}, nil, fmt.Errorf("%s: %w", name, protowire.ParseError(k))
	}
	m := protowire.ConsumeFieldValue(n, typ, b[k:])
	if m < 0 {
		return field{}, nil, fmt.Errorf("%s field %d: %w", name, n, protowire.ParseError(m))
	}
	f := field{number: int32(n), kind: typ, raw: b[k : k+m]}
	if typ == protowire.BytesType {
		v, size := protowire.ConsumeBytes(f.raw)
		if size < 0 {
			return field{}, nil, fmt.Errorf("%s field %d: %w", name, n, protowire.ParseError(size))
		}
		f.data = v
	}
	return f, b[k+m:], nil
}

func bytesValue(f field) []byte {
	return f.data
}

func requireKind(f field, want protowire.Type, name string) error {
	if f.kind != want {
		return fmt.Errorf("%s field %d has wire type %d, want %d", name, f.number, f.kind, want)
	}
	return nil
}

func setBytes(f field, name string, out *[]byte) error {
	if err := requireKind(f, protowire.BytesType, name); err != nil {
		return err
	}
	*out = bytesValue(f)
	return nil
}

func setBool(f field, name string, out *bool) error {
	if err := requireKind(f, protowire.VarintType, name); err != nil {
		return err
	}
	v, size := protowire.ConsumeVarint(f.raw)
	if size < 0 {
		return fmt.Errorf("%s field %d: %w", name, f.number, protowire.ParseError(size))
	}
	*out = v != 0
	return nil
}

func setInt32(f field, name string, out *int32) error {
	if err := requireKind(f, protowire.VarintType, name); err != nil {
		return err
	}
	*out = signed32(f)
	return nil
}

func setInt64(f field, name string, out *int64) error {
	if err := requireKind(f, protowire.VarintType, name); err != nil {
		return err
	}
	*out = signed64(f)
	return nil
}

func setUint32(f field, name string, out *uint32) error {
	if err := requireKind(f, protowire.VarintType, name); err != nil {
		return err
	}
	*out = unsigned32(f)
	return nil
}

func setUint64(f field, name string, out *uint64) error {
	if err := requireKind(f, protowire.VarintType, name); err != nil {
		return err
	}
	v, size := protowire.ConsumeVarint(f.raw)
	if size < 0 {
		return fmt.Errorf("%s field %d: %w", name, f.number, protowire.ParseError(size))
	}
	*out = v
	return nil
}

func appendRaw(dst []byte, n protowire.Number, v []byte) []byte {
	if v == nil {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.BytesType)
	return protowire.AppendBytes(dst, v)
}

func isVariant(f field, first, last int32) bool {
	return f.kind == protowire.BytesType && f.number >= first && f.number <= last
}

func consumeNested[T any](f field, name string, out **T, decode func([]byte, *T) error) error {
	if err := requireKind(f, protowire.BytesType, name); err != nil {
		return err
	}
	*out = new(T)
	return decode(bytesValue(f), *out)
}

func consumeRepeated[T any](f field, name string, out *[]T, decode func([]byte, *T) error) error {
	if err := requireKind(f, protowire.BytesType, name); err != nil {
		return err
	}
	var item T
	if err := decode(bytesValue(f), &item); err != nil {
		return err
	}
	*out = append(*out, item)
	return nil
}

func appendBytesField(f field, name string, out *[][]byte) error {
	if err := requireKind(f, protowire.BytesType, name); err != nil {
		return err
	}
	*out = append(*out, bytesValue(f))
	return nil
}

func appendOptionalMessage[T any](dst []byte, n protowire.Number, m *T, encode func([]byte, *T) []byte) []byte {
	if m == nil {
		return dst
	}
	return appendMessage(dst, n, m, encode)
}

func appendVariant(dst []byte, n protowire.Number, v []byte) []byte {
	dst = protowire.AppendTag(dst, n, protowire.BytesType)
	return protowire.AppendBytes(dst, v)
}

func appendMessage[T any](dst []byte, n protowire.Number, m *T, encode func([]byte, *T) []byte) []byte {
	dst = protowire.AppendTag(dst, n, protowire.BytesType)
	start := len(dst)
	dst = encode(dst, m)
	size := len(dst) - start
	prefix := varintSize(size)
	var pad [binary.MaxVarintLen64]byte
	dst = append(dst, pad[:prefix]...)
	copy(dst[start+prefix:], dst[start:start+size])
	writeVarint(dst[start:start+prefix], size)
	return dst
}

func varintSize(v int) int {
	n := 1
	for v >= 0x80 {
		v >>= 7
		n++
	}
	return n
}

func writeVarint(dst []byte, v int) {
	for i := range dst {
		if v < 0x80 {
			dst[i] = byte(v & 0x7f)
			return
		}
		dst[i] = byte(v&0x7f) | 0x80
		v >>= 7
	}
}

func appendRepeatedBytes(dst []byte, n protowire.Number, values [][]byte) []byte {
	for _, v := range values {
		dst = protowire.AppendTag(dst, n, protowire.BytesType)
		dst = protowire.AppendBytes(dst, v)
	}
	return dst
}

func appendBool(dst []byte, n protowire.Number, v bool) []byte {
	if !v {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.VarintType)
	return protowire.AppendVarint(dst, 1)
}

func appendUint32(dst []byte, n protowire.Number, v uint32) []byte {
	if v == 0 {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.VarintType)
	return protowire.AppendVarint(dst, uint64(v))
}

func appendUint64(dst []byte, n protowire.Number, v uint64) []byte {
	if v == 0 {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.VarintType)
	return protowire.AppendVarint(dst, v)
}

func appendInt32(dst []byte, n protowire.Number, v int32) []byte {
	if v == 0 {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.VarintType)
	return appendSigned32(dst, v)
}

func appendInt64(dst []byte, n protowire.Number, v int64) []byte {
	if v == 0 {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.VarintType)
	return appendSigned64(dst, v)
}

func appendSigned32(dst []byte, v int32) []byte {
	if v >= 0 {
		for v >= 0x80 {
			dst = append(dst, byte(v&0x7f)|0x80)
			v >>= 7
		}
		return append(dst, byte(v&0x7f))
	}
	for range 9 {
		dst = append(dst, byte(v&0x7f)|0x80)
		v >>= 7
	}
	return append(dst, 1)
}

func appendSigned64(dst []byte, v int64) []byte {
	if v >= 0 {
		for v >= 0x80 {
			dst = append(dst, byte(v&0x7f)|0x80)
			v >>= 7
		}
		return append(dst, byte(v&0x7f))
	}
	for range 9 {
		dst = append(dst, byte(v&0x7f)|0x80)
		v >>= 7
	}
	return append(dst, 1)
}

func signed32(f field) int32 {
	var v int32
	for i := 0; i < len(f.raw) && i < 5; i++ {
		v |= int32(f.raw[i]&0x7f) << (7 * i)
	}
	return v
}

func signed64(f field) int64 {
	var v int64
	for i := 0; i < len(f.raw) && i < 9; i++ {
		v |= int64(f.raw[i]&0x7f) << (7 * i)
	}
	if len(f.raw) == 10 {
		v |= int64(f.raw[9]&1) << 63
	}
	return v
}

func unsigned32(f field) uint32 {
	var v uint32
	for i := 0; i < len(f.raw) && i < 5; i++ {
		v |= uint32(f.raw[i]&0x7f) << (7 * i)
	}
	return v
}
