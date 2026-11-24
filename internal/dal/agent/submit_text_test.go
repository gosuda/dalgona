package agent

import (
	"context"
	"errors"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

func TestSubmitRefusesInvalidTextBeforeAnyEffect(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		var calls atomic.Int32
		h := newHarness(t, func(h *harness) {
			h.cfg.Tools = approvingTools(&calls)
		})
		thread := h.start()
		updates := thread.Subscribe()
		defer thread.Unsubscribe(updates)
		before, _, err := thread.Records()
		if err != nil {
			t.Fatal(err)
		}
		for name, text := range map[string]string{"NUL": "keep\x00this", "malformed UTF-8": "bad\xff"} {
			ctx, cancel := context.WithTimeout(t.Context(), time.Minute)
			submitErr := thread.Submit(ctx, Input{Text: text})
			cancel()
			if textErr, ok := errors.AsType[*textinput.Error](submitErr); !ok || textErr.Field != "input" {
				t.Fatalf("%s: Submit = %v, want an input text error", name, submitErr)
			}
		}
		if got := calls.Load(); got != 0 {
			t.Fatalf("tool builds during refused submissions = %d, want none", got)
		}
		requireNoTrustPrompt(t, updates)
		after, _, err := thread.Records()
		if err != nil {
			t.Fatal(err)
		}
		if len(after) != len(before) {
			t.Fatalf("journal records went from %d to %d, want no append", len(before), len(after))
		}
	})
}
