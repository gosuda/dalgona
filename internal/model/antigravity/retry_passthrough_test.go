package antigravity

import (
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/model"
)

func TestNewPassesRetryThrough(t *testing.T) {
	t.Parallel()
	zero := New(Config{})
	if zero.cfg.Retry.StartTimeout != 0 || zero.cfg.Retry.IdleTimeout != 0 {
		t.Fatalf("zero Retry = %+v, want zeros", zero.cfg.Retry)
	}
	explicit := New(Config{Retry: model.RetryOptions{StartTimeout: 30 * time.Second}})
	if explicit.cfg.Retry.StartTimeout != 30*time.Second || explicit.cfg.Retry.IdleTimeout != 0 {
		t.Fatalf("explicit Retry = %+v, want 30s start and zero idle", explicit.cfg.Retry)
	}
}
