package model

import (
	"errors"
	"fmt"
	"io"
	"os"
	"syscall"
	"testing"
)

func TestClassifyAttemptRetriesMarkedTransportText(t *testing.T) {
	t.Parallel()
	eof := AttemptResult{Err: fmt.Errorf("google: read: %w", io.ErrUnexpectedEOF)}
	if got := classifyAttempt(eof); got != Retry {
		t.Fatalf("classifyAttempt(unexpected EOF) = %v, want Retry", got)
	}
	reset := &os.SyscallError{Syscall: "read", Err: syscall.ECONNRESET}
	resetResult := AttemptResult{Err: reset}
	if got := classifyAttempt(resetResult); got != Retry {
		t.Fatalf("classifyAttempt(connection reset) = %v, want Retry", got)
	}
	plain := AttemptResult{Err: errors.New("connection reset by peer")}
	if got := classifyAttempt(plain); got != Terminal {
		t.Fatalf("classifyAttempt(unmarked text) = %v, want Terminal", got)
	}
}
