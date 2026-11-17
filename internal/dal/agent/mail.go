package agent

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"sort"
	"strings"
	"time"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

const (
	mailOutboxBound       = 20
	mailInboxBound        = 20
	mailAggregateWindow   = int64(60_000)
	mailSenderAggregate   = 20
	mailReceiverAggregate = 40
	mailBucketBurst       = 3
	mailBucketRefillMS    = int64(1_000)
)

var (
	ErrMailFull        = errors.New("post: recipient mailbox is full (20)")
	ErrMailRateLimited = errors.New("post: recipient rate limit is 40 messages per 60 seconds")
	ErrMailSenderRate  = errors.New("post: sender rate limit is 20 messages per 60 seconds")
	ErrMailPeerRate    = errors.New("post: recipient rate limit is 3 messages burst and 1 message per second")
)

type MailEndpoint struct {
	ThreadID  string
	SessionID string
	AgentID   string
	State     string
}

type MailOutRequest struct {
	OriginTurnID string
	OperationID  string
	Text         string
	ReplyTo      string
	To           event.MailAddress
}

type MailOut struct {
	Message event.MailMessage
}

type MailReceive struct {
	Message event.MailMessage
}

type MailSettle struct {
	Result event.MailSettled
}

type MailDisposition struct {
	Message event.MailMessage
	Outcome string
	Reason  string
	TurnID  string
}

type MailSnapshot struct {
	Outgoing     []event.MailMessage
	Pending      []event.MailMessage
	RunOnMail    bool
	AutoTurns    uint8
	HeldReason   string
	Dispositions []MailDisposition
}

func mailAddressKey(address event.MailAddress) string {
	return address.SessionID + "/" + address.AgentID
}

func mailMessageID(senderThreadID, originTurnID, operationID string) string {
	compact, err := json.Marshal([3]string{senderThreadID, originTurnID, operationID})
	if err != nil {
		return ""
	}
	sum := sha256.Sum256(compact)
	return "mail-" + hex.EncodeToString(sum[:])
}

func (t *Thread) mailAddress() event.MailAddress {
	return event.MailAddress{SessionID: t.mailSessionID(), AgentID: t.mailAgentID()}
}

func (t *Thread) mailSessionID() string {
	return t.host.SessionRoot(t.id)
}

func (t *Thread) mailAgentID() string {
	if t.created.Parent == "" && t.created.Advisor == "" {
		return "main"
	}
	return t.id
}

func mailClockHighWater(mail *event.MailState, nowMS int64) int64 {
	high := nowMS
	if mail != nil {
		for _, entry := range mail.Messages {
			if entry.Message.ClockMS > high {
				high = entry.Message.ClockMS
			}
		}
	}
	return high
}

func checkMailOutRequest(req MailOutRequest) error {
	if strings.TrimSpace(req.Text) == "" {
		return errors.New("post: text is empty")
	}
	if len(req.Text) > event.MaxMailTextBytes {
		return fmt.Errorf("post: text exceeds %d bytes", event.MaxMailTextBytes)
	}
	if req.OriginTurnID == "" || req.OperationID == "" {
		return errors.New("post: invalid originating operation")
	}
	return nil
}

func maskMailBody(raw string, mask func(string) (string, error)) (string, error) {
	masked, err := mask(raw)
	if err != nil {
		return "", errors.New("post: masking failed")
	}
	if len(masked) > event.MaxMailTextBytes {
		return "", fmt.Errorf("post: text exceeds %d bytes", event.MaxMailTextBytes)
	}
	return masked, nil
}

func mailOutHop(mail *event.MailState, state event.State, originTurnID string) (uint8, error) {
	origin, tracked := mail.TurnHops[originTurnID]
	if !tracked && state.TurnID != originTurnID {
		return 0, errors.New("post: invalid originating operation")
	}
	hop := origin + 1
	if hop > event.MaxMailHop {
		return 0, fmt.Errorf("post: hop limit is %d", event.MaxMailHop)
	}
	return hop, nil
}

func checkMailOutRetry(mail *event.MailState, id string, candidate event.MailMessage) (event.MailMessage, bool, error) {
	existing, ok := mail.Messages[id]
	if !ok {
		return event.MailMessage{}, false, nil
	}
	if !existing.Outgoing {
		return event.MailMessage{}, false, fmt.Errorf("post: persist mail: message %s was not sent by this actor", id)
	}
	probe := existing.Message
	probe.Text = candidate.Text
	probe.ReplyTo = candidate.ReplyTo
	probe.To = candidate.To
	probe.Hop = candidate.Hop
	if event.MailFingerprint(probe) != existing.Fingerprint {
		return event.MailMessage{}, false, errors.New("post: message id conflict")
	}
	return existing.Message, true, nil
}

func settledMailIDs(records []event.Seqd) map[string]bool {
	settled := make(map[string]bool, len(records))
	for _, record := range records {
		if done, ok := record.Event.(event.MailSettled); ok {
			settled[done.ID] = true
		}
	}
	return settled
}

func checkMailOutQuota(mail *event.MailState, settled map[string]bool, clock int64) error {
	if countMailOutstanding(mail, settled) >= mailOutboxBound {
		return errors.New("post: sender outbox is full (20)")
	}
	if countMailSentAggregate(mail, clock) >= mailSenderAggregate {
		return ErrMailSenderRate
	}
	return nil
}

func countMailOutstanding(mail *event.MailState, settled map[string]bool) int {
	outstanding := 0
	for id, entry := range mail.Messages {
		if entry.Outgoing && entry.Outcome == event.MailOutcomeQueued && !settled[id] {
			outstanding++
		}
	}
	return outstanding
}

func countMailSentAggregate(mail *event.MailState, clock int64) int {
	sent := 0
	for _, entry := range mail.Messages {
		if entry.Outgoing && entry.Message.AcceptedMS > clock-mailAggregateWindow && entry.Message.AcceptedMS <= clock {
			sent++
		}
	}
	return sent
}

type mailOutPlan struct {
	mail     *event.MailState
	hop      uint8
	masked   string
	id       string
	clock    int64
	replyTo  string
	existing event.MailMessage
	retry    bool
}

func (t *Thread) planMailOut(req MailOutRequest) (mailOutPlan, error) {
	var plan mailOutPlan
	records, state, err := t.Records()
	if err != nil {
		return plan, err
	}
	mail := state.Mail
	if mail == nil {
		mail = &event.MailState{}
	}
	plan.mail = mail
	hop, err := mailOutHop(mail, state, req.OriginTurnID)
	if err != nil {
		return plan, err
	}
	plan.hop = hop
	masked, err := maskMailBody(req.Text, t.host.mask)
	if err != nil {
		return plan, err
	}
	plan.masked = masked
	plan.id = mailMessageID(t.id, req.OriginTurnID, req.OperationID)
	existing, dup, err := checkMailOutRetry(mail, plan.id, event.MailMessage{Text: masked, ReplyTo: req.ReplyTo, To: req.To, Hop: hop})
	if err != nil {
		return plan, err
	}
	if dup {
		plan.retry = true
		plan.existing = existing
		return plan, nil
	}
	plan.clock = mailClockHighWater(mail, time.Now().UTC().UnixMilli())
	if quotaErr := checkMailOutQuota(mail, settledMailIDs(records), plan.clock); quotaErr != nil {
		return plan, quotaErr
	}
	replyTo, err := t.mailReplyTarget(mail, req.ReplyTo)
	if err != nil {
		return plan, err
	}
	plan.replyTo = replyTo
	return plan, nil
}

func (t *Thread) newMailOutMessage(plan mailOutPlan, req MailOutRequest) event.MailMessage {
	pairKey := event.MailPairKey(t.mailAddress(), req.To)
	return event.MailMessage{
		ID:         plan.id,
		From:       t.mailAddress(),
		To:         req.To,
		PairSeq:    plan.mail.Pairs[pairKey].LastSeq + 1,
		Text:       plan.masked,
		ReplyTo:    plan.replyTo,
		Hop:        plan.hop,
		AcceptedMS: plan.clock,
		ClockMS:    plan.clock,
	}
}

func (t *Thread) MailOut(ctx context.Context, req MailOutRequest) (event.MailMessage, error) {
	if err := checkMailOutRequest(req); err != nil {
		return event.MailMessage{}, err
	}
	t.mailMu.Lock()
	defer t.mailMu.Unlock()
	plan, err := t.planMailOut(req)
	if err != nil {
		return event.MailMessage{}, err
	}
	if plan.retry {
		return plan.existing, nil
	}
	message := t.newMailOutMessage(plan, req)
	if _, err := t.appendEvents(ctx, event.MailOutgoing{Message: message}); err != nil {
		return event.MailMessage{}, fmt.Errorf("post: persist mail: %w", err)
	}
	return message, nil
}

func (t *Thread) mailReplyTarget(mail *event.MailState, replyTo string) (string, error) {
	if replyTo == "" {
		return "", nil
	}
	entry, ok := mail.Messages[replyTo]
	if !ok {
		return "", errors.New("post: unknown reply_to")
	}
	self := mailAddressKey(t.mailAddress())
	if mailAddressKey(entry.Message.From) == self || mailAddressKey(entry.Message.To) == self {
		return replyTo, nil
	}
	return "", errors.New("post: unknown reply_to")
}

func checkMailReceiveHead(message event.MailMessage) error {
	if len(message.Text) > event.MaxMailTextBytes {
		return fmt.Errorf("post: text exceeds %d bytes", event.MaxMailTextBytes)
	}
	if message.Hop < 1 || message.Hop > event.MaxMailHop {
		return fmt.Errorf("post: hop limit is %d", event.MaxMailHop)
	}
	return nil
}

func admitMailReceive(mail *event.MailState, message event.MailMessage, self string) (bool, error) {
	fingerprint := event.MailFingerprint(message)
	if existing, ok := mail.Messages[message.ID]; ok {
		if existing.Fingerprint != fingerprint {
			return false, errors.New("post: message id conflict")
		}
		return false, nil
	}
	if mailAddressKey(message.To) != self {
		return false, fmt.Errorf("post: persist mail: message %s is not addressed to this actor", message.ID)
	}
	if countMailPending(mail) >= mailInboxBound {
		return false, ErrMailFull
	}
	return true, nil
}

func countMailPending(mail *event.MailState) int {
	pending := 0
	for _, entry := range mail.Messages {
		if entry.Received && entry.Outcome == "" {
			pending++
		}
	}
	return pending
}

func admitMailBucket(mail *event.MailState, senderKey string, clock int64) (uint8, int64, error) {
	tokens, refill := mailBucketState(mail, senderKey, clock)
	if elapsed := clock - refill; elapsed >= mailBucketRefillMS {
		refillTokens := uint64(tokens) + uint64(elapsed/mailBucketRefillMS)
		if refillTokens > mailBucketBurst {
			refillTokens = mailBucketBurst
		}
		tokens = uint8(refillTokens)
		refill = clock
	}
	if tokens == 0 {
		return 0, 0, ErrMailPeerRate
	}
	return tokens - 1, refill, nil
}

func mailBucketState(mail *event.MailState, senderKey string, clock int64) (uint8, int64) {
	var latest *event.MailMessage
	for _, entry := range mail.Messages {
		if !entry.Received || mailAddressKey(entry.Message.From) != senderKey {
			continue
		}
		if entry.Message.ClockMS > clock {
			clock = entry.Message.ClockMS
		}
		if latest == nil || entry.Message.ClockMS > latest.ClockMS {
			candidate := entry.Message
			latest = &candidate
		}
	}
	if latest == nil {
		return mailBucketBurst, clock
	}
	refill := latest.BucketRefillMS
	if refill == 0 {
		refill = latest.ClockMS
	}
	return latest.BucketTokens, refill
}

func checkMailReceiveAggregate(mail *event.MailState, clock int64) error {
	admitted := 0
	for _, entry := range mail.Messages {
		if entry.Received && entry.Message.AcceptedMS > clock-mailAggregateWindow && entry.Message.AcceptedMS <= clock {
			admitted++
		}
	}
	if admitted >= mailReceiverAggregate {
		return ErrMailRateLimited
	}
	return nil
}

type mailReceivePlan struct {
	clock  int64
	tokens uint8
	refill int64
	masked string
}

func (t *Thread) planMailReceive(ctx context.Context, message event.MailMessage) (mailReceivePlan, bool, error) {
	var plan mailReceivePlan
	state, err := t.mailState(ctx)
	if err != nil {
		return plan, false, err
	}
	mail := state.Mail
	if mail == nil {
		mail = &event.MailState{}
	}
	proceed, err := admitMailReceive(mail, message, mailAddressKey(t.mailAddress()))
	if err != nil {
		return plan, false, err
	}
	if !proceed {
		return plan, false, nil
	}
	clock := mailClockHighWater(mail, time.Now().UTC().UnixMilli())
	plan.clock = clock
	tokens, refill, err := admitMailBucket(mail, mailAddressKey(message.From), clock)
	if err != nil {
		return plan, false, err
	}
	plan.tokens = tokens
	plan.refill = refill
	if aggErr := checkMailReceiveAggregate(mail, clock); aggErr != nil {
		return plan, false, aggErr
	}
	masked, err := maskMailBody(message.Text, t.host.mask)
	if err != nil {
		return plan, false, err
	}
	plan.masked = masked
	return plan, true, nil
}

func (t *Thread) MailReceive(ctx context.Context, message event.MailMessage) error {
	if err := checkMailReceiveHead(message); err != nil {
		return err
	}
	t.mailMu.Lock()
	defer t.mailMu.Unlock()
	plan, proceed, err := t.planMailReceive(ctx, message)
	if err != nil {
		return err
	}
	if !proceed {
		return nil
	}
	message.Text = plan.masked
	message.ClockMS = plan.clock
	message.BucketTokens = plan.tokens
	message.BucketRefillMS = plan.refill
	if _, err := t.appendEvents(ctx, event.MailReceived{Message: message}); err != nil {
		return fmt.Errorf("post: persist mail: %w", err)
	}
	return nil
}

func checkMailSettlement(mail *event.MailState, result event.MailSettled) error {
	if mail == nil {
		return nil
	}
	entry, ok := mail.Messages[result.ID]
	if !ok {
		return nil
	}
	if entry.Outcome != event.MailOutcomeDelivered && entry.Outcome != event.MailOutcomeRefused {
		return nil
	}
	if entry.Outcome != result.Outcome {
		return errors.New("post: settlement conflict")
	}
	return nil
}

func (t *Thread) MailSettle(ctx context.Context, result event.MailSettled) error {
	t.mailMu.Lock()
	defer t.mailMu.Unlock()
	state, err := t.mailState(ctx)
	if err != nil {
		return err
	}
	if err := checkMailSettlement(state.Mail, result); err != nil {
		return err
	}
	if _, err := t.appendEvents(ctx, result); err != nil {
		return fmt.Errorf("post: persist mail: %w", err)
	}
	return nil
}

func (t *Thread) MailSnapshot(ctx context.Context) (MailSnapshot, error) {
	var snapshot MailSnapshot
	v, err := t.view(ctx)
	if err != nil {
		return MailSnapshot{}, err
	}
	mail := v.state.Mail
	if mail == nil {
		return snapshot, nil
	}
	snapshot.RunOnMail = mail.RunOnMail
	snapshot.AutoTurns = mail.AutoTurns
	snapshot.HeldReason = mail.HeldReason
	ids := make([]string, 0, len(mail.Messages))
	for id := range mail.Messages {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	for _, id := range ids {
		entry := mail.Messages[id]
		if entry.Outgoing {
			snapshot.Outgoing = append(snapshot.Outgoing, entry.Message)
		}
		if entry.Received {
			outcome := entry.Outcome
			if outcome == "" {
				snapshot.Pending = append(snapshot.Pending, entry.Message)
				outcome = "pending"
			}
			snapshot.Dispositions = append(snapshot.Dispositions, MailDisposition{
				Message: entry.Message, Outcome: outcome, TurnID: entry.ConsumedTurn,
			})
		}
	}
	return snapshot, nil
}

func (t *Thread) SetRunOnMail(ctx context.Context, enabled bool) error {
	t.mailMu.Lock()
	defer t.mailMu.Unlock()
	v, err := t.view(ctx)
	if err != nil {
		return err
	}
	if v.state.Mail != nil && v.state.Mail.RunOnMail == enabled {
		return nil
	}
	if _, err := t.appendEvents(ctx, event.MailRunSet{Enabled: enabled}); err != nil {
		return fmt.Errorf("post: persist mail: %w", err)
	}
	return nil
}

func (t *Thread) mailState(ctx context.Context) (event.State, error) {
	v, err := t.view(ctx)
	if err != nil {
		return event.State{}, err
	}
	return v.state, nil
}

func MailBinding(ctx context.Context) (*Thread, string, bool) {
	w, err := turnWorker(ctx)
	if err != nil {
		return nil, "", false
	}
	return w.t, w.turnID, true
}
