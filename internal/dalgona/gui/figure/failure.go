package figure

import (
	"errors"
	"fmt"
)

type FailureCode uint8

const (
	FailureInvalid FailureCode = iota + 1
	FailureForbidden
	FailureLimit
	FailureTimeout
	FailureCancelled
	FailureEngine
	FailureProtocol
)

var failureNames = [...]string{
	FailureInvalid:   "invalid-input",
	FailureForbidden: "forbidden-resource",
	FailureLimit:     "limit-exceeded",
	FailureTimeout:   "timeout",
	FailureCancelled: "cancelled",
	FailureEngine:    "engine-failure",
	FailureProtocol:  "protocol-failure",
}

func (c FailureCode) String() string {
	if c == 0 || int(c) >= len(failureNames) {
		return fmt.Sprintf("failure-%d", c)
	}
	return failureNames[c]
}

type Span struct {
	Line      int
	Column    int
	EndLine   int
	EndColumn int
}

func (s Span) Known() bool { return s.Line > 0 }

type Failure struct {
	Code    FailureCode
	Message string
	Span    Span
}

func (f *Failure) Error() string {
	if f.Span.Known() {
		return fmt.Sprintf("line %d, column %d: %s", f.Span.Line, f.Span.Column, f.Message)
	}
	return f.Message
}

func failAt(code FailureCode, span Span, format string, args ...any) *Failure {
	return &Failure{Code: code, Message: fmt.Sprintf(format, args...), Span: span}
}

func fail(code FailureCode, format string, args ...any) *Failure {
	return &Failure{Code: code, Message: fmt.Sprintf(format, args...)}
}

func FailureOf(err error) *Failure {
	if err == nil {
		return nil
	}
	var failure *Failure
	if errors.As(err, &failure) {
		return failure
	}
	return &Failure{Code: FailureEngine, Message: err.Error()}
}
