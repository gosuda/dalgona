package platform

import "context"

func awaitNative[T any](ctx context.Context, call func() T) (T, error) {
	var zero T
	if err := ctx.Err(); err != nil {
		return zero, err
	}
	done := make(chan T, 1)
	go func() { done <- call() }()
	select {
	case <-ctx.Done():
		return zero, ctx.Err()
	case result := <-done:
		return result, nil
	}
}
