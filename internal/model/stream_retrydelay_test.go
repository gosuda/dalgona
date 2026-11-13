package model

import (
	"testing"
	"time"
)

func TestRetryDelayHonorsLargeRetryAfter(t *testing.T) {
	t.Parallel()
	if got := RetryDelay(0, 90*time.Second, nil); got != 90*time.Second {
		t.Fatalf("RetryDelay(0, 90s) = %v, want 90s", got)
	}
	if got := RetryDelay(0, 60*time.Second, nil); got != 60*time.Second {
		t.Fatalf("RetryDelay(0, 60s) = %v, want 60s", got)
	}
	if got := RetryDelay(1, time.Hour, nil); got != time.Hour {
		t.Fatalf("RetryDelay(1, 1h) = %v, want 1h", got)
	}
}
