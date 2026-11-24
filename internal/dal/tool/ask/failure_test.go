package ask

import (
	"context"
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
)

type replyAsker struct {
	reply Reply
	err   error
}

func (a replyAsker) Ask(context.Context, string, []Question) (Reply, error) { return a.reply, a.err }

var nameQuestion = []byte(`{"questions":[{"header":"Name","question":"What should the service be called?"}]}`)

func TestAskDeliveryFailureIsInternal(t *testing.T) {
	deliveryErr := errors.New("question delivery failed")
	result, err := New(replyAsker{err: deliveryErr}).Run(t.Context(), tool.Call{ID: "ask", Args: nameQuestion}, tool.Env{})
	if !errors.Is(err, deliveryErr) {
		t.Fatalf("error = %v, want the delivery error", err)
	}
	if result.Failure != tool.FailureInternal {
		t.Fatalf("failure = %q, want a delivery failure marked internal", result.Failure)
	}
}

func TestAskOrdinaryOutcomesStayOrdinary(t *testing.T) {
	tests := []struct {
		name      string
		args      []byte
		asker     replyAsker
		cancelled bool
		fails     bool
	}{
		{name: "invalid arguments", args: []byte(`{"questions":[]}`), fails: true},
		{name: "declined", args: nameQuestion, asker: replyAsker{reply: Reply{Outcome: OutcomeFailed, Detail: "the client sent an unreadable answer"}}},
		{name: "interrupted", args: nameQuestion, asker: replyAsker{reply: Reply{Outcome: OutcomeInterrupted}}},
		{name: "delivery failure of a cancelled call", args: nameQuestion, asker: replyAsker{err: errors.New("thread closed")}, cancelled: true, fails: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			ctx, cancel := context.WithCancel(t.Context())
			defer cancel()
			if test.cancelled {
				cancel()
			}
			result, err := New(test.asker).Run(ctx, tool.Call{ID: "ask", Args: test.args}, tool.Env{})
			if (err != nil) != test.fails || result.OK {
				t.Fatalf("result = %+v, %v; want a failed ask", result, err)
			}
			if result.Failure != tool.FailureNone {
				t.Fatalf("failure = %q, want an ordinary ask outcome", result.Failure)
			}
		})
	}
}
