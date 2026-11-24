package figure

import (
	"encoding/binary"
	"math"
	"strings"
	"testing"
)

func appendUvarint(b []byte, v uint64) []byte {
	return binary.AppendUvarint(b, v)
}

func appendF32(b []byte, v float32) []byte {
	return binary.LittleEndian.AppendUint32(b, math.Float32bits(v))
}

func emptyListHeader() []byte {
	b := []byte(displayMagic)
	b = appendF32(b, 1)
	b = appendF32(b, 1)
	return appendUvarint(b, 0)
}

func TestDecodeListRejectsSectionBeyondOperationLimit(t *testing.T) {
	b := emptyListHeader()
	b = appendUvarint(b, 1_000_000)
	if _, err := DecodeList(b); err == nil || !strings.Contains(err.Error(), "entries") {
		t.Fatalf("DecodeList with 1M labels = %v, want an entry-count refusal", err)
	}
}

func TestDecodeListRejectsAggregateBeyondDecodedBudget(t *testing.T) {
	b := emptyListHeader()
	b = appendUvarint(b, 200_000)
	b = append(b, make([]byte, 200_000)...)
	b = appendUvarint(b, 200_000)
	for range 200_000 {
		b = append(b, 0, 0)
	}
	b = appendUvarint(b, 0)
	b = appendUvarint(b, 200_000)
	for range 200_000 {
		b = append(b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
	}
	b = appendUvarint(b, 1)
	b = append(b, make([]byte, 19)...)
	if _, err := DecodeList(b); err == nil || !strings.Contains(err.Error(), "budget") {
		t.Fatalf("DecodeList with 600k entries plus one tile = %v, want a budget refusal", err)
	}
}

func TestDecodeListAcceptsBoundedList(t *testing.T) {
	l := &List{Width: 8, Height: 4}
	l.Labels = []string{"caption"}
	l.Ops = []Op{{Kind: OpFill, Index: 0, Paint: SolidPaint(Color{R: 1, G: 2, B: 3, A: 4})}}
	l.Paths = []Path{{Verbs: []Verb{VerbMove}, Points: []Point{{X: 1, Y: 2}}}}
	got, err := DecodeList(EncodeList(l))
	if err != nil {
		t.Fatalf("DecodeList of a bounded list failed: %v", err)
	}
	if got.Operations() != l.Operations() || len(got.Labels) != 1 {
		t.Fatalf("decoded list = %+v, want the encoded operations and label", got)
	}
}

func TestValidateRejectsExcessiveStateNesting(t *testing.T) {
	l := &List{Width: 8, Height: 4}
	for range StateDepthLimit + 1 {
		l.Ops = append(l.Ops, Op{Kind: OpOpacity, Alpha: 1})
	}
	if err := l.Validate(); err == nil || !strings.Contains(err.Error(), "nest") {
		t.Fatalf("Validate with %d nested states = %v, want a nesting refusal", len(l.Ops), err)
	}
}

func TestValidateAcceptsStateLimitNesting(t *testing.T) {
	l := &List{Width: 8, Height: 4}
	for range StateDepthLimit {
		l.Ops = append(l.Ops, Op{Kind: OpOpacity, Alpha: 1})
	}
	for range StateDepthLimit {
		l.Ops = append(l.Ops, Op{Kind: OpPop})
	}
	if err := l.Validate(); err != nil {
		t.Fatalf("Validate with %d balanced states failed: %v", StateDepthLimit, err)
	}
}
