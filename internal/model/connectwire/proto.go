package connectwire

import (
	"encoding/binary"
	"fmt"

	"google.golang.org/protobuf/encoding/protowire"
)

type Field struct {
	Number int32
	Kind   protowire.Type
	Raw    []byte
	Data   []byte
}

func ConsumeFields(b []byte, name string, fn func(Field) error) error {
	for len(b) > 0 {
		f, rest, err := ConsumeField(b, name)
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

func ConsumeField(b []byte, name string) (Field, []byte, error) {
	n, typ, k := protowire.ConsumeTag(b)
	if k < 0 {
		return Field{}, nil, fmt.Errorf("%s: %w", name, protowire.ParseError(k))
	}
	m := protowire.ConsumeFieldValue(n, typ, b[k:])
	if m < 0 {
		return Field{}, nil, fmt.Errorf("%s field %d: %w", name, n, protowire.ParseError(m))
	}
	f := Field{Number: int32(n), Kind: typ, Raw: b[k : k+m]}
	if typ == protowire.BytesType {
		v, size := protowire.ConsumeBytes(f.Raw)
		if size < 0 {
			return Field{}, nil, fmt.Errorf("%s field %d: %w", name, n, protowire.ParseError(size))
		}
		f.Data = v
	}
	return f, b[k+m:], nil
}

func BytesValue(f Field) []byte {
	return f.Data
}

func RequireKind(f Field, want protowire.Type, name string) error {
	if f.Kind != want {
		return fmt.Errorf("%s field %d has wire type %d, want %d", name, f.Number, f.Kind, want)
	}
	return nil
}

func SetBytes(f Field, name string, out *[]byte) error {
	if err := RequireKind(f, protowire.BytesType, name); err != nil {
		return err
	}
	*out = BytesValue(f)
	return nil
}

func SetBool(f Field, name string, out *bool) error {
	if err := RequireKind(f, protowire.VarintType, name); err != nil {
		return err
	}
	v, size := protowire.ConsumeVarint(f.Raw)
	if size < 0 {
		return fmt.Errorf("%s field %d: %w", name, f.Number, protowire.ParseError(size))
	}
	*out = v != 0
	return nil
}

func SetInt32(f Field, name string, out *int32) error {
	if err := RequireKind(f, protowire.VarintType, name); err != nil {
		return err
	}
	*out = Signed32(f)
	return nil
}

func SetInt64(f Field, name string, out *int64) error {
	if err := RequireKind(f, protowire.VarintType, name); err != nil {
		return err
	}
	*out = Signed64(f)
	return nil
}

func SetUint32(f Field, name string, out *uint32) error {
	if err := RequireKind(f, protowire.VarintType, name); err != nil {
		return err
	}
	*out = Unsigned32(f)
	return nil
}

func SetUint64(f Field, name string, out *uint64) error {
	if err := RequireKind(f, protowire.VarintType, name); err != nil {
		return err
	}
	v, size := protowire.ConsumeVarint(f.Raw)
	if size < 0 {
		return fmt.Errorf("%s field %d: %w", name, f.Number, protowire.ParseError(size))
	}
	*out = v
	return nil
}

func AppendRaw(dst []byte, n protowire.Number, v []byte) []byte {
	if v == nil {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.BytesType)
	return protowire.AppendBytes(dst, v)
}

func IsVariant(f Field, first, last int32) bool {
	return f.Kind == protowire.BytesType && f.Number >= first && f.Number <= last
}

func ConsumeNested[T any](f Field, name string, out **T, decode func([]byte, *T) error) error {
	if err := RequireKind(f, protowire.BytesType, name); err != nil {
		return err
	}
	*out = new(T)
	return decode(BytesValue(f), *out)
}

func ConsumeRepeated[T any](f Field, name string, out *[]T, decode func([]byte, *T) error) error {
	if err := RequireKind(f, protowire.BytesType, name); err != nil {
		return err
	}
	var item T
	if err := decode(BytesValue(f), &item); err != nil {
		return err
	}
	*out = append(*out, item)
	return nil
}

func AppendBytesField(f Field, name string, out *[][]byte) error {
	if err := RequireKind(f, protowire.BytesType, name); err != nil {
		return err
	}
	*out = append(*out, BytesValue(f))
	return nil
}

func AppendOptionalMessage[T any](dst []byte, n protowire.Number, m *T, encode func([]byte, *T) []byte) []byte {
	if m == nil {
		return dst
	}
	return AppendMessage(dst, n, m, encode)
}

func AppendVariant(dst []byte, n protowire.Number, v []byte) []byte {
	dst = protowire.AppendTag(dst, n, protowire.BytesType)
	return protowire.AppendBytes(dst, v)
}

func AppendMessage[T any](dst []byte, n protowire.Number, m *T, encode func([]byte, *T) []byte) []byte {
	dst = protowire.AppendTag(dst, n, protowire.BytesType)
	start := len(dst)
	dst = encode(dst, m)
	size := len(dst) - start
	prefix := VarintSize(size)
	var pad [binary.MaxVarintLen64]byte
	dst = append(dst, pad[:prefix]...)
	copy(dst[start+prefix:], dst[start:start+size])
	WriteVarint(dst[start:start+prefix], size)
	return dst
}

func VarintSize(v int) int {
	n := 1
	for v >= 0x80 {
		v >>= 7
		n++
	}
	return n
}

func WriteVarint(dst []byte, v int) {
	for i := range dst {
		if v < 0x80 {
			dst[i] = byte(v & 0x7f)
			return
		}
		dst[i] = byte(v&0x7f) | 0x80
		v >>= 7
	}
}

func AppendRepeatedBytes(dst []byte, n protowire.Number, values [][]byte) []byte {
	for _, v := range values {
		dst = protowire.AppendTag(dst, n, protowire.BytesType)
		dst = protowire.AppendBytes(dst, v)
	}
	return dst
}

func AppendBool(dst []byte, n protowire.Number, v bool) []byte {
	if !v {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.VarintType)
	return protowire.AppendVarint(dst, 1)
}

func AppendUint32(dst []byte, n protowire.Number, v uint32) []byte {
	if v == 0 {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.VarintType)
	return protowire.AppendVarint(dst, uint64(v))
}

func AppendUint64(dst []byte, n protowire.Number, v uint64) []byte {
	if v == 0 {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.VarintType)
	return protowire.AppendVarint(dst, v)
}

func AppendInt32(dst []byte, n protowire.Number, v int32) []byte {
	if v == 0 {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.VarintType)
	return AppendSigned32(dst, v)
}

func AppendInt64(dst []byte, n protowire.Number, v int64) []byte {
	if v == 0 {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.VarintType)
	return AppendSigned64(dst, v)
}

func AppendSigned32(dst []byte, v int32) []byte {
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

func AppendSigned64(dst []byte, v int64) []byte {
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

func Signed32(f Field) int32 {
	var v int32
	for i := 0; i < len(f.Raw) && i < 5; i++ {
		v |= int32(f.Raw[i]&0x7f) << (7 * i)
	}
	return v
}

func Signed64(f Field) int64 {
	var v int64
	for i := 0; i < len(f.Raw) && i < 9; i++ {
		v |= int64(f.Raw[i]&0x7f) << (7 * i)
	}
	if len(f.Raw) == 10 {
		v |= int64(f.Raw[9]&1) << 63
	}
	return v
}

func Unsigned32(f Field) uint32 {
	var v uint32
	for i := 0; i < len(f.Raw) && i < 5; i++ {
		v |= uint32(f.Raw[i]&0x7f) << (7 * i)
	}
	return v
}
