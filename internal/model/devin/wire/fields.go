package wire

import (
	"fmt"
	"math"

	"github.com/gosuda/dalgona/internal/model/connectwire"
	"google.golang.org/protobuf/encoding/protowire"
)

func setFloat32(f connectwire.Field, name string, out *float32) error {
	if err := connectwire.RequireKind(f, protowire.Fixed32Type, name); err != nil {
		return err
	}
	v, size := protowire.ConsumeFixed32(f.Raw)
	if size < 0 {
		return fmt.Errorf("%s field %d: %w", name, f.Number, protowire.ParseError(size))
	}
	*out = math.Float32frombits(v)
	return nil
}

func setFloat64(f connectwire.Field, name string, out *float64) error {
	if err := connectwire.RequireKind(f, protowire.Fixed64Type, name); err != nil {
		return err
	}
	v, size := protowire.ConsumeFixed64(f.Raw)
	if size < 0 {
		return fmt.Errorf("%s field %d: %w", name, f.Number, protowire.ParseError(size))
	}
	*out = math.Float64frombits(v)
	return nil
}

func appendPackedInt32(dst []byte, n protowire.Number, values []int32) []byte {
	if len(values) == 0 {
		return dst
	}
	return connectwire.AppendMessage(dst, n, &values, appendSigned32List)
}

func appendSigned32List(dst []byte, values *[]int32) []byte {
	for _, v := range *values {
		dst = connectwire.AppendSigned32(dst, v)
	}
	return dst
}

func appendPresentInt32(dst []byte, n protowire.Number, v *int32) []byte {
	if v == nil {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.VarintType)
	return connectwire.AppendSigned32(dst, *v)
}

func setPresentInt32(f connectwire.Field, name string, out **int32) error {
	if err := connectwire.RequireKind(f, protowire.VarintType, name); err != nil {
		return err
	}
	v := connectwire.Signed32(f)
	*out = &v
	return nil
}

func appendFloat32(dst []byte, n protowire.Number, v float32) []byte {
	if v == 0 {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.Fixed32Type)
	return protowire.AppendFixed32(dst, math.Float32bits(v))
}

func appendFloat64(dst []byte, n protowire.Number, v float64) []byte {
	if v == 0 {
		return dst
	}
	dst = protowire.AppendTag(dst, n, protowire.Fixed64Type)
	return protowire.AppendFixed64(dst, math.Float64bits(v))
}
