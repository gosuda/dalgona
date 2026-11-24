//go:build gui

package access

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestActivationInvalidatesWhenTheTargetChanges(t *testing.T) {
	var changed Actions
	changed.ActivationKey = "new-target"
	changed.textSince = 7
	queued := platform.AccessibilityIntent{Action: platform.ActionActivate, Revision: 3}
	if !changed.stale(queued) {
		t.Fatal("a queued activation survives its target being replaced")
	}
	fresh := platform.AccessibilityIntent{Action: platform.ActionActivate, Revision: 7}
	if changed.stale(fresh) {
		t.Fatal("a current activation is reported stale")
	}
	var plain Actions
	plain.textSince = 7
	if plain.stale(queued) {
		t.Fatal("an action without a target key is reported stale")
	}
}
