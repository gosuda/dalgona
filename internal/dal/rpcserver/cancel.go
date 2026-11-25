package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	MetadataDeadline = 10 * time.Second
	PageDeadline     = 30 * time.Second
	deadlineStage    = "deadline"
)

var errDeadline = errors.New("rpcserver: request deadline passed")

func deadline(limit time.Duration, m method) method {
	return func(s *server, ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
		return bounded(ctx, limit, func(ctx context.Context) (jsontext.Value, error) { return m(s, ctx, params) })
	}
}

func WithDeadline(limit time.Duration, ext Extension) Extension {
	return func(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
		return bounded(ctx, limit, func(ctx context.Context) (jsontext.Value, error) { return ext(ctx, params) })
	}
}

func bounded(ctx context.Context, limit time.Duration, call func(context.Context) (jsontext.Value, error)) (jsontext.Value, error) {
	scoped, release := rpc.RequestScope(ctx)
	defer release()
	timed, cancel := context.WithTimeoutCause(scoped, limit, errDeadline)
	defer cancel()
	result, err := call(timed)
	if err == nil {
		return result, nil
	}
	if _, native := rpc.ErrorDataOf(err); native || timed.Err() == nil {
		return nil, err
	}
	if errors.Is(context.Cause(timed), errDeadline) {
		return nil, rpc.NativeError(rpc.ErrorData{Kind: rpc.ErrorCancelled, Stage: deadlineStage})
	}
	return nil, rpc.NativeKind(rpc.ErrorCancelled)
}
