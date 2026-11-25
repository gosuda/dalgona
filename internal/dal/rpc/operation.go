package rpc

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"strconv"
	"time"
	"uuid"
)

const (
	OperationMember      = "dalgonaOperationId"
	OperationPast        = 24 * time.Hour
	OperationFuture      = 5 * time.Minute
	operationVersion     = 7
	operationVariantMask = 0xC0
	operationVariant     = 0x80
)

type Operation struct {
	Method      string
	OperationID string
	Endpoint    string
	Generation  uint64
}

type UnknownOutcomeError struct {
	Method      string
	Endpoint    string
	OperationID string
	Generation  uint64
	Cause       error
}

func (e *UnknownOutcomeError) Error() string {
	return fmt.Sprintf("%s %s on %s (generation %d) has an unknown outcome: %v", e.Method, e.OperationID, e.Endpoint, e.Generation, e.Cause)
}

func (e *UnknownOutcomeError) Unwrap() error { return e.Cause }

func NewOperationID() string { return uuid.NewV7().String() }

func OperationTime(id string) (time.Time, error) {
	parsed, err := uuid.Parse(id)
	switch {
	case err != nil:
		return time.Time{}, fmt.Errorf("operation id %q: %w", id, err)
	case parsed.String() != id:
		return time.Time{}, fmt.Errorf("operation id %q is not canonical", id)
	case parsed[6]>>4 != operationVersion || parsed[8]&operationVariantMask != operationVariant:
		return time.Time{}, fmt.Errorf("operation id %q is not a UUIDv7", id)
	}
	var millis int64
	for _, b := range parsed[:6] {
		millis = millis<<8 | int64(b)
	}
	return time.UnixMilli(millis), nil
}

func (c *Conn) CallOperation(ctx context.Context, op Operation, params any, result any) error {
	pv, err := marshalParams(params)
	if err != nil {
		return err
	}
	withID, err := withOperationID(pv, op.OperationID)
	if err != nil {
		return err
	}
	idv := jsontext.Value(strconv.AppendUint(nil, c.nextID(), 10))
	dispatched, err := c.roundTrip(ctx, outgoing{id: idv, method: op.Method, params: withID}, result)
	if dispatched && (errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) || errors.Is(err, io.ErrClosedPipe)) {
		return &UnknownOutcomeError{Method: op.Method, Endpoint: op.Endpoint, OperationID: op.OperationID, Generation: op.Generation, Cause: err}
	}
	return err
}

func withOperationID(params jsontext.Value, id string) (jsontext.Value, error) {
	member, err := jsontext.AppendQuote([]byte(`{"`+OperationMember+`":`), id)
	if err != nil {
		return nil, fmt.Errorf("encode operation id: %w", err)
	}
	trimmed := bytes.TrimSpace(params)
	switch {
	case len(trimmed) == 0 || bytes.Equal(trimmed, []byte("null")):
		return append(member, '}'), nil
	case trimmed[0] != '{':
		return nil, errors.New("operation params must be a JSON object")
	}
	rest := bytes.TrimSpace(trimmed[1:])
	if len(rest) > 0 && rest[0] != '}' {
		member = append(member, ',')
	}
	return append(member, rest...), nil
}
