package sandboxsvc

import (
	"errors"
	"fmt"
	"testing"
)

func TestIsGone(t *testing.T) {
	if IsGone(nil) {
		t.Fatal("IsGone(nil) = true, want false")
	}
	if IsGone(errors.New("boom")) {
		t.Fatal("IsGone(boom) = true, want false")
	}
	if !IsGone(ErrGone) {
		t.Fatal("IsGone(ErrGone) = false, want true")
	}
	if !IsGone(fmt.Errorf("docker: remove: %w", ErrGone)) {
		t.Fatal("IsGone(wrapped ErrGone) = false, want true")
	}
}
