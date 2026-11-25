package rpcserver

import (
	"context"
	"encoding/json/jsontext"
)

func liveOperationResult(effect method, live Extension) method {
	materialize := extensionMethod(live)
	return func(s *server, ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
		result, err := effect(s, ctx, params)
		if err != nil {
			return nil, err
		}
		return materialize(s, ctx, result)
	}
}

func NativeFeature(ctx context.Context, name string) int {
	s, ok := ctx.Value(serverKey{}).(*server)
	if !ok || s.native.Load() == nil {
		return 0
	}
	for _, feature := range nativeFeatures {
		if feature.name == name && feature.offered(s) {
			return feature.version
		}
	}
	return 0
}
