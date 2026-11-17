package event

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"maps"

	json "encoding/json/v2"
)

const (
	TypeMailOutgoing    = "mail.outgoing"
	TypeMailReceived    = "mail.received"
	TypeMailConsumed    = "mail.consumed"
	TypeMailSettled     = "mail.settled"
	TypeMailRunSet      = "mail.run_set"
	TypeMailAutoStarted = "mail.auto_started"
	TypeMailHumanReset  = "mail.human_reset"
	TypeMailHopAdvanced = "mail.hop_advanced"
	TypeMailHeld        = "mail.held"
)

const (
	MailOutcomeQueued    = "queued"
	MailOutcomeDelivered = "delivered"
	MailOutcomeRefused   = "refused"
)

const (
	MailHeldTurnFailed = "turn_failed_or_interrupted"
)

const (
	MaxMailTextBytes  = 16 << 10
	MaxMailHop        = 8
	mailIDPrefix      = "mail-"
	mailIDPrefixBytes = len(mailIDPrefix)
)

type MailAddress struct {
	SessionID string
	AgentID   string
}

type MailMessage struct {
	ID             string
	From           MailAddress
	To             MailAddress
	PairSeq        uint64
	Text           string
	ReplyTo        string
	Hop            uint8
	AcceptedMS     int64
	ClockMS        int64
	BucketTokens   uint8
	BucketRefillMS int64
}

type MailOutgoing struct {
	Message MailMessage
}

type MailReceived struct {
	Message MailMessage
}

type MailConsumed struct {
	IDs    []string
	TurnID string
	Hop    uint8
}

type MailSettled struct {
	ID              string
	Outcome         string
	Reason          string
	RecipientTurnID string
}

type MailRunSet struct {
	Enabled bool
}

type MailAutoStarted struct {
	TurnID string
}

type MailHumanReset struct {
	TurnID string
}

type MailHopAdvanced struct {
	TurnID string
	Hop    uint8
}

type MailHeld struct {
	Reason string
}

func (MailOutgoing) Type() string      { return TypeMailOutgoing }
func (MailReceived) Type() string      { return TypeMailReceived }
func (MailConsumed) Type() string      { return TypeMailConsumed }
func (MailSettled) Type() string       { return TypeMailSettled }
func (MailRunSet) Type() string        { return TypeMailRunSet }
func (MailAutoStarted) Type() string   { return TypeMailAutoStarted }
func (MailHumanReset) Type() string    { return TypeMailHumanReset }
func (MailHopAdvanced) Type() string   { return TypeMailHopAdvanced }
func (MailHeld) Type() string          { return TypeMailHeld }
func (e MailOutgoing) Validate() error { return e.Message.Validate() }

func (e MailReceived) Validate() error { return e.Message.Validate() }

func (e MailConsumed) Validate() error {
	if e.TurnID == "" || len(e.IDs) == 0 {
		return errors.New("mail.consumed needs turn_id and at least one id")
	}
	if e.Hop < 1 || e.Hop > MaxMailHop {
		return fmt.Errorf("mail.consumed hop must be between 1 and %d", MaxMailHop)
	}
	for i, id := range e.IDs {
		if !validMailID(id) {
			return fmt.Errorf("mail.consumed ids[%d] must be a mail id", i)
		}
	}
	return nil
}

func (e MailSettled) Validate() error {
	if !validMailID(e.ID) {
		return errors.New("mail.settled id must be a mail id")
	}
	if err := validateMailOutcome(e.Outcome); err != nil {
		return err
	}
	if e.Outcome == MailOutcomeDelivered && e.RecipientTurnID == "" {
		return errors.New("mail.settled delivered needs recipient_turn_id")
	}
	return nil
}

func (e MailRunSet) Validate() error { return nil }

func (e MailAutoStarted) Validate() error {
	return required(map[string]string{"turn_id": e.TurnID})
}

func (e MailHumanReset) Validate() error {
	return required(map[string]string{"turn_id": e.TurnID})
}

func (e MailHopAdvanced) Validate() error {
	if err := required(map[string]string{"turn_id": e.TurnID}); err != nil {
		return err
	}
	if e.Hop < 1 || e.Hop > MaxMailHop {
		return fmt.Errorf("mail.hop_advanced hop must be between 1 and %d", MaxMailHop)
	}
	return nil
}

func (e MailHeld) Validate() error {
	return oneOf("reason", e.Reason, MailHeldTurnFailed)
}

func (m MailMessage) Validate() error {
	if !validMailID(m.ID) {
		return errors.New("mail message id must be a mail id")
	}
	if err := validateMailAddress("from", m.From); err != nil {
		return err
	}
	if err := validateMailAddress("to", m.To); err != nil {
		return err
	}
	if m.PairSeq < 1 {
		return errors.New("mail message pair_seq must be positive")
	}
	if len(m.Text) > MaxMailTextBytes {
		return fmt.Errorf("mail message text exceeds %d bytes", MaxMailTextBytes)
	}
	if m.ReplyTo != "" && !validMailID(m.ReplyTo) {
		return errors.New("mail message reply_to must be a mail id")
	}
	if m.Hop > MaxMailHop {
		return fmt.Errorf("mail message hop must be at most %d", MaxMailHop)
	}
	return nil
}

func validateMailAddress(name string, address MailAddress) error {
	if address.SessionID == "" || address.AgentID == "" {
		return fmt.Errorf("mail message %s needs session_id and agent_id", name)
	}
	return nil
}

func validMailID(id string) bool {
	return len(id) > mailIDPrefixBytes && id[:mailIDPrefixBytes] == mailIDPrefix
}

func validateMailOutcome(outcome string) error {
	return oneOf("outcome", outcome, MailOutcomeQueued, MailOutcomeDelivered, MailOutcomeRefused)
}

func mailMembers(e Event) ([]member, error) {
	if m, handled := mailMembersCore(e); handled {
		return m, nil
	}
	return mailMembersRest(e)
}

func mailMembersCore(e Event) ([]member, bool) {
	switch value := e.(type) {
	case MailOutgoing:
		return mailMessageMembers(value.Message), true
	case MailReceived:
		return mailMessageMembers(value.Message), true
	case MailConsumed:
		return mailConsumedMembers(value), true
	case MailSettled:
		return mailSettledMembers(value), true
	case MailRunSet:
		return []member{field("enabled", boolValue(value.Enabled))}, true
	}
	return nil, false
}

func mailConsumedMembers(value MailConsumed) []member {
	return []member{
		field("ids", arrayValue(value.IDs, stringValue)),
		field("turn_id", stringValue(value.TurnID)),
		field("hop", uint8Value(value.Hop)),
	}
}

func mailSettledMembers(value MailSettled) []member {
	fields := []member{
		field("id", stringValue(value.ID)), field("outcome", stringValue(value.Outcome)),
	}
	fields = optionalString(fields, "reason", value.Reason)
	return optionalString(fields, "recipient_turn_id", value.RecipientTurnID)
}

func mailMembersRest(e Event) ([]member, error) {
	switch value := e.(type) {
	case MailAutoStarted:
		return []member{field("turn_id", stringValue(value.TurnID))}, nil
	case MailHumanReset:
		return []member{field("turn_id", stringValue(value.TurnID))}, nil
	case MailHopAdvanced:
		return []member{
			field("turn_id", stringValue(value.TurnID)), field("hop", uint8Value(value.Hop)),
		}, nil
	case MailHeld:
		return []member{field("reason", stringValue(value.Reason))}, nil
	default:
		return nil, fmt.Errorf("event: %T does not match type %s", e, e.Type())
	}
}

func mailMessageMembers(message MailMessage) []member {
	fields := []member{
		field("id", stringValue(message.ID)),
		field("from", mailAddressValue(message.From)),
		field("to", mailAddressValue(message.To)),
		field("pair_seq", uint64Value(message.PairSeq)),
		field("text", stringValue(message.Text)),
		field("hop", uint8Value(message.Hop)),
		field("accepted_ms", int64Value(message.AcceptedMS)),
	}
	fields = optionalString(fields, "reply_to", message.ReplyTo)
	fields = optionalInt64(fields, "clock_ms", message.ClockMS)
	if message.BucketTokens != 0 {
		fields = append(fields, field("bucket_tokens", uint8Value(message.BucketTokens)))
	}
	return optionalInt64(fields, "bucket_refill_ms", message.BucketRefillMS)
}

func mailAddressValue(address MailAddress) jsontext.Value {
	return objectValue(
		field("session_id", stringValue(address.SessionID)),
		field("agent_id", stringValue(address.AgentID)),
	)
}

func decodeMailEvent(kind string, r *objectReader) (Event, error) {
	if e, handled := decodeMailEventCore(kind, r); handled {
		return e, nil
	}
	return decodeMailEventRest(kind, r)
}

func decodeMailEventCore(kind string, r *objectReader) (Event, bool) {
	switch kind {
	case TypeMailOutgoing:
		return MailOutgoing{Message: decodeMailMessage(r)}, true
	case TypeMailReceived:
		return MailReceived{Message: decodeMailMessage(r)}, true
	case TypeMailConsumed:
		return decodeMailConsumed(r), true
	case TypeMailSettled:
		return MailSettled{
			ID: r.str("id"), Outcome: r.str("outcome"), Reason: r.str("reason"),
			RecipientTurnID: r.str("recipient_turn_id"),
		}, true
	}
	return nil, false
}

func decodeMailEventRest(kind string, r *objectReader) (Event, error) {
	switch kind {
	case TypeMailRunSet:
		return MailRunSet{Enabled: r.boolean("enabled")}, nil
	case TypeMailAutoStarted:
		return MailAutoStarted{TurnID: r.str("turn_id")}, nil
	case TypeMailHumanReset:
		return MailHumanReset{TurnID: r.str("turn_id")}, nil
	case TypeMailHopAdvanced:
		return MailHopAdvanced{TurnID: r.str("turn_id"), Hop: r.u8("hop")}, nil
	case TypeMailHeld:
		return MailHeld{Reason: r.str("reason")}, nil
	default:
		return nil, fmt.Errorf("unsupported mail event %q", kind)
	}
}

func decodeMailMessage(r *objectReader) MailMessage {
	message := MailMessage{
		ID: r.str("id"), PairSeq: r.uint64("pair_seq"), Text: r.str("text"),
		ReplyTo: r.str("reply_to"), Hop: r.u8("hop"), AcceptedMS: r.int64("accepted_ms"),
		ClockMS: r.int64("clock_ms"), BucketTokens: r.u8("bucket_tokens"), BucketRefillMS: r.int64("bucket_refill_ms"),
	}
	message.From = decodeMailAddress(r, "from")
	message.To = decodeMailAddress(r, "to")
	return message
}

func decodeMailAddress(r *objectReader, name string) MailAddress {
	fields, err := decodeObject(r.raw(name))
	if err != nil {
		r.err = err
		return MailAddress{}
	}
	reader := objectReader{fields: fields}
	address := MailAddress{SessionID: reader.str("session_id"), AgentID: reader.str("agent_id")}
	if reader.err != nil {
		r.err = reader.err
	}
	return address
}

func decodeMailConsumed(r *objectReader) MailConsumed {
	var ids []string
	r.array("ids", func(raw jsontext.Value) error {
		value, err := decodeString(raw)
		ids = append(ids, value)
		return err
	})
	return MailConsumed{IDs: ids, TurnID: r.str("turn_id"), Hop: r.u8("hop")}
}

type MailEntry struct {
	Message      MailMessage
	Fingerprint  string
	Received     bool
	Outgoing     bool
	ConsumedTurn string
	Outcome      string
}

type MailPairState struct {
	LastSeq uint64
}

type MailState struct {
	Messages   map[string]MailEntry
	Pairs      map[string]MailPairState
	RunOnMail  bool
	AutoTurns  uint8
	Held       bool
	HeldReason string
	TurnHops   map[string]uint8
}

func (s *State) ensureMail() *MailState {
	if s.Mail == nil {
		s.Mail = &MailState{}
	}
	if s.Mail.Messages == nil {
		s.Mail.Messages = make(map[string]MailEntry)
	}
	if s.Mail.Pairs == nil {
		s.Mail.Pairs = make(map[string]MailPairState)
	}
	if s.Mail.TurnHops == nil {
		s.Mail.TurnHops = make(map[string]uint8)
	}
	return s.Mail
}

func MailPairKey(from, to MailAddress) string {
	return from.SessionID + "\x00" + from.AgentID + "\x00" + to.SessionID + "\x00" + to.AgentID
}

func MailFingerprint(message MailMessage) string {
	payload := struct {
		ID      string      `json:"id"`
		From    MailAddress `json:"from"`
		To      MailAddress `json:"to"`
		PairSeq uint64      `json:"pair_seq"`
		Text    string      `json:"text"`
		ReplyTo string      `json:"reply_to"`
		Hop     uint8       `json:"hop"`
	}{
		ID: message.ID, From: message.From, To: message.To, PairSeq: message.PairSeq,
		Text: message.Text, ReplyTo: message.ReplyTo, Hop: message.Hop,
	}
	raw, err := json.Marshal(payload)
	if err != nil {
		return ""
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:])
}

func foldMailEvent(s *State, seq uint64, e Event) error {
	if err, handled := foldMailEventCore(s, seq, e); handled {
		return err
	}
	return foldMailEventRest(s, seq, e)
}

func foldMailEventCore(s *State, seq uint64, e Event) (error, bool) {
	switch value := e.(type) {
	case MailOutgoing:
		return foldMailOutgoing(s, value.Message), true
	case MailReceived:
		return foldMailReceived(s, value.Message), true
	case MailConsumed:
		return foldMailConsumed(s, seq, value), true
	case MailSettled:
		return foldMailSettled(s, value), true
	case MailRunSet:
		s.ensureMail().RunOnMail = value.Enabled
		return nil, true
	}
	return nil, false
}

func foldMailEventRest(s *State, seq uint64, e Event) error {
	switch value := e.(type) {
	case MailAutoStarted:
		s.ensureMail().AutoTurns++
		return nil
	case MailHumanReset:
		return foldMailHumanReset(s)
	case MailHopAdvanced:
		return foldMailHopAdvanced(s, value)
	case MailHeld:
		mail := s.ensureMail()
		mail.Held = true
		mail.HeldReason = value.Reason
		return nil
	default:
		return fmt.Errorf("event: fold: %T does not match %s", e, e.Type())
	}
}

func foldMailHumanReset(s *State) error {
	mail := s.ensureMail()
	mail.AutoTurns = 0
	mail.Held = false
	mail.HeldReason = ""
	return nil
}

func foldMailOutgoing(s *State, message MailMessage) error {
	mail := s.ensureMail()
	key := MailPairKey(message.From, message.To)
	pair := mail.Pairs[key]
	if existing, ok := mail.Messages[message.ID]; ok {
		if existing.Fingerprint != MailFingerprint(message) {
			return fmt.Errorf("event: fold: mail message id conflict %s", message.ID)
		}
		if !existing.Outgoing {
			return fmt.Errorf("event: fold: mail message %s already received", message.ID)
		}
		return nil
	}
	if message.PairSeq <= pair.LastSeq {
		return fmt.Errorf("event: fold: mail pair_seq %d does not follow %d", message.PairSeq, pair.LastSeq)
	}
	mail.Pairs[key] = MailPairState{LastSeq: message.PairSeq}
	mail.Messages[message.ID] = MailEntry{Message: message, Fingerprint: MailFingerprint(message), Outgoing: true, Outcome: MailOutcomeQueued}
	return nil
}

func foldMailReceived(s *State, message MailMessage) error {
	mail := s.ensureMail()
	fingerprint := MailFingerprint(message)
	if existing, ok := mail.Messages[message.ID]; ok {
		if existing.Fingerprint != fingerprint {
			return fmt.Errorf("event: fold: mail message id conflict %s", message.ID)
		}
		if existing.Received {
			return nil
		}
		if existing.Outgoing {
			return fmt.Errorf("event: fold: mail message %s already sent by this thread", message.ID)
		}
		return fmt.Errorf("event: fold: mail message %s has terminal outcome %s", message.ID, existing.Outcome)
	}
	mail.Messages[message.ID] = MailEntry{Message: message, Fingerprint: fingerprint, Received: true}
	return nil
}

func foldMailConsumed(s *State, seq uint64, e MailConsumed) error {
	if err := requireTurn(s, e.TurnID); err != nil {
		return err
	}
	mail := s.ensureMail()
	for _, id := range e.IDs {
		entry, ok := mail.Messages[id]
		if !ok || !entry.Received || entry.ConsumedTurn != "" {
			return fmt.Errorf("event: fold: mail.consumed for unknown or consumed message %s", id)
		}
		if e.Hop < entry.Message.Hop {
			return fmt.Errorf("event: fold: mail.consumed hop %d below message hop %d", e.Hop, entry.Message.Hop)
		}
		entry.ConsumedTurn = e.TurnID
		entry.Outcome = MailOutcomeDelivered
		mail.Messages[id] = entry
	}
	if current := mail.TurnHops[e.TurnID]; e.Hop > current {
		mail.TurnHops[e.TurnID] = e.Hop
	}
	return nil
}

func foldMailSettled(s *State, e MailSettled) error {
	mail := s.ensureMail()
	entry, ok := mail.Messages[e.ID]
	if !ok {
		entry = MailEntry{Message: MailMessage{ID: e.ID}}
	}
	if entry.Outcome == MailOutcomeDelivered || entry.Outcome == MailOutcomeRefused {
		if entry.Outcome == e.Outcome {
			return nil
		}
		return fmt.Errorf("event: fold: mail message %s already settled %s", e.ID, entry.Outcome)
	}
	if entry.Outcome == e.Outcome {
		return nil
	}
	entry.Outcome = e.Outcome
	mail.Messages[e.ID] = entry
	return nil
}

func foldMailHopAdvanced(s *State, e MailHopAdvanced) error {
	if s.TurnID == "" || s.TurnID != e.TurnID {
		return fmt.Errorf("event: fold: mail.hop_advanced turn %s does not match open turn %s", e.TurnID, s.TurnID)
	}
	mail := s.ensureMail()
	if current := mail.TurnHops[e.TurnID]; e.Hop > current {
		mail.TurnHops[e.TurnID] = e.Hop
	}
	return nil
}

func cloneMailState(mail *MailState) *MailState {
	if mail == nil {
		return nil
	}
	clone := *mail
	clone.Messages = maps.Clone(mail.Messages)
	clone.Pairs = maps.Clone(mail.Pairs)
	clone.TurnHops = maps.Clone(mail.TurnHops)
	return &clone
}
