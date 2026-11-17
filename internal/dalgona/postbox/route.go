package postbox

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

type parsedAddress struct {
	raw      string
	main     bool
	graphID  string
	nodeID   string
	external string
}

func parseAddress(raw string) (parsedAddress, error) {
	if raw == "" || len(raw) > 192 {
		return parsedAddress{}, errInvalidAddress(raw)
	}
	if !isPrintableAddress(raw) {
		return parsedAddress{}, errInvalidAddress(raw)
	}
	if raw == "main" {
		return parsedAddress{raw: raw, main: true}, nil
	}
	if head, tail, found := strings.Cut(raw, "/"); found {
		return parseGraphAddress(raw, head, tail)
	}
	if strings.ContainsAny(raw, "/\\:*?\"<>|") {
		return parsedAddress{}, errInvalidAddress(raw)
	}
	return parsedAddress{raw: raw, external: raw}, nil
}

func isPrintableAddress(raw string) bool {
	for i := 0; i < len(raw); i++ {
		c := raw[i]
		if c > 127 || c <= 32 || c == 127 {
			return false
		}
	}
	return true
}

func parseGraphAddress(raw, head, tail string) (parsedAddress, error) {
	if !validGraphID(head) || !validNodeID(tail) {
		return parsedAddress{}, errInvalidAddress(raw)
	}
	return parsedAddress{raw: raw, graphID: head, nodeID: tail}, nil
}

func errInvalidAddress(raw string) error {
	return fmt.Errorf("post: invalid address %q", raw)
}

func validGraphID(id string) bool {
	digits, ok := graphDigits(id)
	if !ok {
		return false
	}
	return isLowerHex(digits)
}

func graphDigits(id string) (string, bool) {
	if len(id) < 3 || len(id) > 18 || id[0] != 'g' || id[1] != '-' {
		return "", false
	}
	digits := id[2:]
	if len(digits) > 16 || digits[0] == '0' {
		return "", false
	}
	return digits, true
}

func isLowerHex(digits string) bool {
	for i := 0; i < len(digits); i++ {
		if isNotLowerHex(digits[i]) {
			return false
		}
	}
	return true
}

func isNotLowerHex(c byte) bool {
	return (c < '0' || c > '9') && (c < 'a' || c > 'f')
}

func validNodeID(id string) bool {
	if len(id) < 1 || len(id) > 48 {
		return false
	}
	if id[0] < 'a' || id[0] > 'z' {
		return false
	}
	for i := 1; i < len(id); i++ {
		if !isNodeChar(id[i]) {
			return false
		}
	}
	return true
}

func isNodeChar(c byte) bool {
	return (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_' || c == '-'
}

type resolvedRecipient struct {
	threadID string
	thread   *agent.Thread
	address  event.MailAddress
	state    string
}

func (s *Service) handle(pending routedPost) postResult {
	s.mu.Lock()
	receipt, recipientID, err := s.deliverLocked(pending)
	wake := s.continueForMail
	s.mu.Unlock()
	if err == nil && wake != nil {
		_ = wake(pending.ctx, recipientID)
	}
	return postResult{receipt: receipt, err: err}
}

func (s *Service) deliverLocked(pending routedPost) (Receipt, string, error) {
	to := pending.request.To
	parsed, err := parseAddress(to)
	if err != nil {
		return Receipt{}, "", err
	}
	recipient, err := s.resolveLocked(pending.ctx, parsed, pending.from.SessionID)
	if err != nil {
		return Receipt{}, "", err
	}
	if recipient.threadID == pending.senderID {
		return Receipt{}, "", errors.New("post: cannot send to self")
	}
	if err := checkRecipientState(to, recipient.state); err != nil {
		return Receipt{}, "", err
	}
	return s.sendLocked(pending, recipient)
}

func checkRecipientState(to, state string) error {
	switch state {
	case StateFinished:
		return fmt.Errorf("post: recipient %q is finished", to)
	case StateFailed:
		return fmt.Errorf("post: recipient %q is failed", to)
	case StateCrashed:
		return fmt.Errorf("post: recipient %q crashed", to)
	case StateArchived:
		return fmt.Errorf("post: recipient %q is archived", to)
	case StateQueued, StateRunning, StateIdle:
		return nil
	default:
		return fmt.Errorf("post: recipient %q is archived", to)
	}
}

func (s *Service) sendLocked(pending routedPost, recipient resolvedRecipient) (Receipt, string, error) {
	message, err := pending.sender.MailOut(pending.ctx, agent.MailOutRequest{
		OriginTurnID: pending.senderTurn,
		OperationID:  pending.request.OperationID,
		Text:         pending.request.Text,
		ReplyTo:      pending.request.ReplyTo,
		To:           recipient.address,
	})
	if err != nil {
		return Receipt{}, "", err
	}
	if err := recipient.thread.MailReceive(pending.ctx, message); err != nil {
		return Receipt{}, "", err
	}
	settled := event.MailSettled{ID: message.ID, Outcome: event.MailOutcomeQueued}
	if err := pending.sender.MailSettle(pending.ctx, settled); err != nil {
		return Receipt{}, "", errors.New("post: receipt pending; retry the same message")
	}
	return Receipt{
		MessageID: message.ID,
		PairSeq:   message.PairSeq,
		Kind:      ReceiptBuffered,
		Delivery:  "queued",
	}, recipient.threadID, nil
}

func (s *Service) resolveLocked(ctx context.Context, parsed parsedAddress, senderRoot string) (resolvedRecipient, error) {
	switch {
	case parsed.main:
		return s.resolveMain(ctx, senderRoot)
	case parsed.graphID != "":
		return resolvedRecipient{}, fmt.Errorf("post: unknown recipient %q", parsed.raw)
	default:
		return s.resolveExternal(ctx, parsed.external)
	}
}

func (s *Service) resolveMain(ctx context.Context, root string) (resolvedRecipient, error) {
	thread, err := s.host.Resume(ctx, root)
	if err != nil {
		return resolvedRecipient{}, fmt.Errorf("post: unknown recipient %q", "main")
	}
	state, err := s.endpointStateLocked(root)
	if err != nil {
		return resolvedRecipient{}, err
	}
	return resolvedRecipient{
		threadID: root,
		thread:   thread,
		address:  event.MailAddress{SessionID: root, AgentID: "main"},
		state:    state,
	}, nil
}

func (s *Service) resolveExternal(ctx context.Context, threadID string) (resolvedRecipient, error) {
	if !s.sessions {
		return resolvedRecipient{}, fmt.Errorf("post: unknown recipient %q", threadID)
	}
	top := false
	for _, info := range s.host.AllThreads() {
		if info.ID != threadID {
			continue
		}
		top = info.Parent == "" && info.Advisor == ""
	}
	if !top {
		return resolvedRecipient{}, fmt.Errorf("post: unknown recipient %q", threadID)
	}
	thread, err := s.host.Resume(ctx, threadID)
	if err != nil {
		return resolvedRecipient{}, fmt.Errorf("post: unknown recipient %q", threadID)
	}
	state, err := s.endpointStateLocked(threadID)
	if err != nil {
		return resolvedRecipient{}, err
	}
	return resolvedRecipient{
		threadID: threadID,
		thread:   thread,
		address:  event.MailAddress{SessionID: threadID, AgentID: "main"},
		state:    state,
	}, nil
}

func (s *Service) endpointStateLocked(threadID string) (string, error) {
	if endpoint, ok := s.byThread[threadID]; ok {
		return endpoint.State, nil
	}
	state, err := s.deriveState(threadID)
	if err != nil {
		return "", fmt.Errorf("post: unknown recipient %q", threadID)
	}
	return state, nil
}
