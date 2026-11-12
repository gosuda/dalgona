package app

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestEditJudgeFollowsConfig(t *testing.T) {
	t.Parallel()
	off := &judges{cfg: config.Config{}}
	if off.editJudge() != nil {
		t.Fatal("editJudge is set while [edit_review] enabled is false")
	}
	on := &judges{cfg: config.Config{EditReview: config.EditReviewConfig{Enabled: true}}}
	if on.editJudge() == nil {
		t.Fatal("editJudge is nil while [edit_review] enabled is true")
	}
}
