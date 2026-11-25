package agent

import "context"

type operationKey struct{}

func WithOperation(ctx context.Context, operationID string) context.Context {
	return context.WithValue(ctx, operationKey{}, operationID)
}

func operationOf(ctx context.Context) string {
	id, _ := ctx.Value(operationKey{}).(string)
	return id
}
