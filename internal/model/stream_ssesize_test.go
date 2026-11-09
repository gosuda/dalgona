package model

import (
	"errors"
	"strings"
	"testing"
)

func TestReadSSELineTooLarge(t *testing.T) {
	t.Parallel()
	big := strings.Repeat("a", maxSSELineBytes+1)
	err := ReadSSE(t.Context(), strings.NewReader(big), func(_, _ []byte) error {
		t.Error("handler must not run for an oversize line")
		return nil
	})
	if !errors.Is(err, ErrSSELineTooLarge) {
		t.Fatalf("ReadSSE() error = %v, want %v", err, ErrSSELineTooLarge)
	}
}

func TestReadSSEEventTooLarge(t *testing.T) {
	t.Parallel()
	var sb strings.Builder
	chunk := strings.Repeat("b", 8192)
	for sb.Len() <= maxSSEEventBytes {
		sb.WriteString("data: " + chunk + "\n")
	}
	sb.WriteString("\n")
	err := ReadSSE(t.Context(), strings.NewReader(sb.String()), func(_, _ []byte) error {
		t.Error("handler must not run for an oversize event")
		return nil
	})
	if !errors.Is(err, ErrSSEEventTooLarge) {
		t.Fatalf("ReadSSE() error = %v, want %v", err, ErrSSEEventTooLarge)
	}
}
