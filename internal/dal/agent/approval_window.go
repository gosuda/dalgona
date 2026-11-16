package agent

import "fmt"

type pendingApproval struct {
	replies chan Reply
	refused map[<-chan Update]struct{}
}

func newPendingApproval(replies chan Reply) *pendingApproval {
	return &pendingApproval{replies: replies, refused: map[<-chan Update]struct{}{}}
}

func (t *Thread) CannotAnswerApproval(requestID string, window <-chan Update) error {
	return askErr(t, func(a *actor) error {
		pending, ok := a.approvals[requestID]
		if !ok {
			return fmt.Errorf("%w: %s", ErrUnknownRequest, requestID)
		}
		if !a.attached(window) {
			return nil
		}
		pending.refused[window] = struct{}{}
		a.settleApprovals()
		return nil
	})
}

func (a *actor) settleApprovals() {
	for requestID, pending := range a.approvals {
		if a.allRefused(pending.refused) {
			a.resolveApproval(requestID, ReplyDecline)
		}
	}
}

func (a *actor) resolveApproval(requestID string, reply Reply) {
	pending, ok := a.approvals[requestID]
	if !ok {
		return
	}
	delete(a.approvals, requestID)
	select {
	case pending.replies <- reply:
	default:
	}
}
