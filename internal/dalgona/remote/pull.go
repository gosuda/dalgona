package remote

import "context"

func (b *Binder) Pull(ctx context.Context, threadID string) error {
	_, err := b.PullReport(ctx, threadID, nil)
	return err
}
