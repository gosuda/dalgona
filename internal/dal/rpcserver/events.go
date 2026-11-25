package rpcserver

import (
	"bytes"
	"container/list"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"maps"
	"os"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

const (
	liveReferenceLimit    = 4096
	liveMetadataLimit     = 4 << 20
	liveIdleLimit         = 5 * time.Minute
	metadataStringLimit   = 4096
	itemHeaderLimit       = 64 << 10
	activeMemoryLimit     = 256 << 10
	sourceLive            = "live"
	sourceSnapshot        = "snapshot"
	encodingUTF8          = "utf8"
	encodingJSON          = "json"
	itemRevisionHexLength = 32
	spillBlobPrefix       = "blobs/"
)

var errContentDrift = errors.New("rpcserver: referenced content changed")

var bulkMembers = map[string]bool{
	"text": true, "aggregatedOutput": true, "command": true, "arguments": true,
	"result": true, "changes": true, "content": true, "contentItems": true,
}

var itemIdentity = map[string]bool{"id": true, "type": true}

type itemField struct {
	name     string
	encoding string
	data     []byte
	blob     *event.BlobRef
	spill    *rpc.OutputSpill
}

func (f itemField) total() int64 {
	switch {
	case f.blob != nil:
		return f.blob.Bytes
	case f.spill != nil:
		return f.spill.Bytes
	}
	return int64(len(f.data))
}

func (f itemField) digest() string {
	switch {
	case f.blob != nil:
		return f.blob.Blob
	case f.spill != nil:
		return f.spill.SHA256
	}
	return digestOf(f.data)
}

type paramsBody struct {
	Metadata map[string]jsontext.Value `json:"metadata"`
	Contents []rpc.ContentReference    `json:"contents"`
}

func itemFields(it rpc.Item, blobs fieldBlobs) (map[string]jsontext.Value, []itemField, error) {
	raw, err := rpc.Marshal(it)
	if err != nil {
		return nil, nil, fmt.Errorf("encode item %s: %w", it.ID, err)
	}
	metadata, fields, err := splitMembers(it.Type, raw, itemIdentity)
	if err != nil {
		return nil, nil, fmt.Errorf("project item %s: %w", it.ID, err)
	}
	if it.Spill != nil {
		delete(metadata, "outputSpill")
		fields = append(fields, itemField{name: "outputSpill", encoding: encodingUTF8, spill: it.Spill})
	}
	for _, name := range slices.Sorted(maps.Keys(blobs)) {
		delete(metadata, name)
		fields = slices.DeleteFunc(fields, func(field itemField) bool { return field.name == name })
		fields = append(fields, itemField{name: name, encoding: blobEncoding(name), blob: blobs[name]})
	}
	return metadata, fields, nil
}

func blobEncoding(name string) string {
	if name == fieldArguments {
		return encodingJSON
	}
	return encodingUTF8
}

func splitMembers(kind string, raw jsontext.Value, skip map[string]bool) (map[string]jsontext.Value, []itemField, error) {
	metadata := map[string]jsontext.Value{}
	var fields []itemField
	err := rpc.ReadMembers(jsontext.NewDecoder(bytes.NewReader(raw)), func(name string, d *jsontext.Decoder) error {
		value, err := d.ReadValue()
		if err != nil || skip[name] {
			return err
		}
		if kind == rpc.KindUser && name == fieldContent {
			text, rest, splitErr := userContent(value)
			fields = append(fields, itemField{name: fieldText, encoding: encodingUTF8, data: text})
			if len(rest) > 0 {
				metadata[name] = rest
			}
			return splitErr
		}
		field, bulk, err := splitMember(kind, name, value)
		if bulk {
			fields = append(fields, field)
		} else {
			metadata[name] = bytes.Clone(value)
		}
		return err
	})
	return metadata, fields, err
}

func userContent(value jsontext.Value) ([]byte, jsontext.Value, error) {
	var parts []jsontext.Value
	if err := json.Unmarshal(value, &parts); err != nil {
		return nil, nil, err
	}
	var text []byte
	rest := make([]jsontext.Value, 0, len(parts))
	for _, part := range parts {
		var entry struct {
			Type string `json:"type"`
			Text string `json:"text"`
		}
		if err := json.Unmarshal(part, &entry); err != nil {
			return nil, nil, err
		}
		if entry.Type == fieldText {
			text = append(text, entry.Text...)
			continue
		}
		rest = append(rest, part)
	}
	if len(rest) == 0 {
		return text, nil, nil
	}
	kept, err := json.Marshal(rest)
	return text, kept, err
}

func splitMember(kind, name string, value jsontext.Value) (itemField, bool, error) {
	if kind == rpc.KindReasoning && name == fieldContent {
		return reasoningField(name, value)
	}
	if value.Kind() == '"' {
		return stringField(name, value)
	}
	field, bulk := jsonField(name, value)
	return field, bulk, nil
}

func reasoningField(name string, value jsontext.Value) (itemField, bool, error) {
	var parts []string
	if err := json.Unmarshal(value, &parts); err != nil {
		return itemField{}, false, err
	}
	return itemField{name: name, encoding: encodingUTF8, data: []byte(strings.Join(parts, ""))}, true, nil
}

func stringField(name string, value jsontext.Value) (itemField, bool, error) {
	var text string
	if err := json.Unmarshal(value, &text); err != nil {
		return itemField{}, false, err
	}
	bulk := bulkMembers[name] || len(text) > metadataStringLimit
	return itemField{name: name, encoding: encodingUTF8, data: []byte(text)}, bulk, nil
}

func jsonField(name string, value jsontext.Value) (itemField, bool) {
	bulk := bulkMembers[name] || isBulkJSON(value)
	return itemField{name: name, encoding: encodingJSON, data: bytes.Clone(value)}, bulk
}

func isBulkJSON(value jsontext.Value) bool {
	if value.Kind() != '{' && value.Kind() != '[' {
		return false
	}
	return len(value) > metadataStringLimit
}

func digestOf(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

func itemHeader(it rpc.Item, blobs fieldBlobs, reference func(itemField) (rpc.ContentReference, error)) (rpc.ItemHeader, error) {
	metadata, fields, err := itemFields(it, blobs)
	if err != nil {
		return rpc.ItemHeader{}, err
	}
	header := rpc.ItemHeader{ID: it.ID, Type: it.Type, Metadata: metadata, Contents: make([]rpc.ContentReference, 0, len(fields))}
	for _, field := range fields {
		ref, refErr := reference(field)
		if refErr != nil {
			return rpc.ItemHeader{}, refErr
		}
		header.Contents = append(header.Contents, ref)
	}
	header.ItemRevision = itemRevision(header)
	encoded, err := json.Marshal(header)
	if err != nil {
		return rpc.ItemHeader{}, fmt.Errorf("encode item header %s: %w", it.ID, err)
	}
	if len(encoded) > itemHeaderLimit {
		return rpc.ItemHeader{}, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	return header, nil
}

func itemRevision(header rpc.ItemHeader) string {
	digest := sha256.New()
	digest.Write([]byte(header.Type + "\x00" + header.ID))
	for _, name := range slices.Sorted(maps.Keys(header.Metadata)) {
		digest.Write([]byte("\x00" + name + "\x00"))
		digest.Write(header.Metadata[name])
	}
	for _, ref := range header.Contents {
		digest.Write([]byte("\x00" + ref.Field + "\x00" + ref.Digest))
	}
	return hex.EncodeToString(digest.Sum(nil))[:itemRevisionHexLength]
}

func fieldReference(source rpc.ContentSource, contentID string, field itemField) (rpc.ContentReference, error) {
	total := field.total()
	if total < 0 {
		return rpc.ContentReference{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return rpc.ContentReference{
		Source: source, ContentID: contentID, Field: field.name, Encoding: field.encoding,
		Total: rpc.FormatDecimal(uint64(total)), Digest: field.digest(),
	}, nil
}

func fieldOfItem(it rpc.Item, blobs fieldBlobs, name string) (itemField, bool, error) {
	_, fields, err := itemFields(it, blobs)
	if err != nil {
		return itemField{}, false, err
	}
	for _, field := range fields {
		if field.name == name {
			return field, true, nil
		}
	}
	return itemField{}, false, nil
}

type liveRef struct {
	key      string
	threadID string
	turnID   string
	itemID   string
	field    string
	head     uint64
	ordinal  uint64
	blob     *event.BlobRef
	spill    *rpc.OutputSpill
	content  frozenContent
	total    int64
	digest   string
	used     time.Time
	cost     int
	element  *list.Element
}

type liveContents struct {
	mu     sync.Mutex
	refs   map[string]*liveRef
	order  *list.List
	bytes  int
	closed bool
}

func newLiveContents() *liveContents {
	return &liveContents{refs: map[string]*liveRef{}, order: list.New()}
}

func (c *liveContents) register(ref *liveRef) (string, error) {
	ref.prepare()
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed || ref.cost > liveMetadataLimit {
		return "", rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	c.makeRoomLocked(1, ref.cost)
	c.insertLocked(ref)
	return ref.key, nil
}

func (r *liveRef) prepare() {
	r.key = newHandle()
	r.used = time.Now()
	r.cost = len(r.key) + len(r.threadID) + len(r.turnID) + len(r.itemID) + len(r.field) + len(r.digest) + 128
}

func (c *liveContents) makeRoomLocked(count, cost int) {
	for len(c.refs)+count > liveReferenceLimit || c.bytes+cost > liveMetadataLimit {
		c.dropLocked(c.order.Front().Value.(*liveRef))
	}
}

func (c *liveContents) insertLocked(ref *liveRef) {
	ref.element = c.order.PushBack(ref)
	c.refs[ref.key] = ref
	c.bytes += ref.cost
}

func (c *liveContents) registerSpilled(refs []*liveRef) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	cost := 0
	for _, ref := range refs {
		cost += ref.cost
	}
	if c.closed || len(refs) > liveReferenceLimit || cost > liveMetadataLimit {
		return rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	c.makeRoomLocked(len(refs), cost)
	for _, ref := range refs {
		c.insertLocked(ref)
	}
	return nil
}

func (c *liveContents) dropLocked(ref *liveRef) {
	c.order.Remove(ref.element)
	delete(c.refs, ref.key)
	c.bytes -= ref.cost
	ref.releaseContent()
}

func (c *liveContents) bind(refs []*liveRef, ordinal uint64) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, ref := range refs {
		ref.ordinal = ordinal
	}
}

func (c *liveContents) withdraw(refs []*liveRef) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, ref := range refs {
		if c.refs[ref.key] == ref {
			c.dropLocked(ref)
		}
	}
}

func (c *liveContents) lookup(key string, now time.Time) (liveRef, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	ref, ok := c.refs[key]
	if !ok {
		return liveRef{}, rpc.NativeKind(rpc.ErrorExpired)
	}
	if now.Sub(ref.used) > liveIdleLimit {
		c.dropLocked(ref)
		return liveRef{}, rpc.NativeKind(rpc.ErrorExpired)
	}
	ref.used = now
	c.order.MoveToBack(ref.element)
	if ref.content.acc != nil {
		ref.content.acc.mu.Lock()
		ref.content.acc.pins++
		ref.content.acc.mu.Unlock()
	}
	return *ref, nil
}

func (r *liveRef) releaseContent() {
	if r.content.acc != nil {
		r.content.release()
	}
}

func (c *liveContents) close() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.closed = true
	for _, ref := range c.refs {
		c.dropLocked(ref)
	}
}

func (c *liveContents) forgetThread(threadID string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, ref := range c.refs {
		if ref.threadID == threadID {
			c.dropLocked(ref)
		}
	}
}

type activeItem struct {
	id     string
	turnID string
	kind   string
	field  string
	acc    *accumulator
}

type liveProjection struct {
	threadID string
	items    []*activeItem
	closed   bool
}

type accumulator struct {
	mu       sync.Mutex
	memory   []byte
	spill    *os.File
	path     string
	size     int64
	overflow bool
	ended    bool
	pins     int
	threadID string
}

type frozenContent struct {
	acc    *accumulator
	length int64
}

func activeField(kind string) string {
	switch kind {
	case rpc.KindAgent:
		return "text"
	case rpc.KindReasoning:
		return "content"
	case rpc.KindCommand:
		return "aggregatedOutput"
	}
	return ""
}

func (p *liveProjection) begin(it *rpc.Item, turnID string) {
	field := activeField(it.Type)
	if p.closed || field == "" || p.find(it.ID) != nil {
		return
	}
	p.items = append(p.items, &activeItem{id: it.ID, turnID: turnID, kind: it.Type, field: field, acc: &accumulator{threadID: p.threadID}})
}

func (p *liveProjection) find(id string) *activeItem {
	for _, item := range p.items {
		if item.id == id {
			return item
		}
	}
	return nil
}

func (p *liveProjection) end(id string) {
	p.drop(func(item *activeItem) bool { return item.id == id })
}

func (p *liveProjection) endTurn(turnID string) {
	p.drop(func(item *activeItem) bool { return item.turnID == turnID })
}

func (p *liveProjection) drop(match func(*activeItem) bool) {
	p.items = slices.DeleteFunc(p.items, func(item *activeItem) bool {
		if !match(item) {
			return false
		}
		item.acc.finish()
		return true
	})
}

func (p *liveProjection) append(id, delta string) {
	if item := p.find(id); item != nil {
		item.acc.append([]byte(delta))
	}
}

func (p *liveProjection) close() {
	for _, item := range p.items {
		item.acc.finish()
	}
	p.items = nil
	p.closed = true
}

func (a *accumulator) append(data []byte) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.overflow || len(data) == 0 {
		return
	}
	room := min(len(data), max(activeMemoryLimit-len(a.memory), 0))
	if a.size == int64(len(a.memory)) && room > 0 {
		a.memory = append(a.memory, data[:room]...)
		a.size += int64(room)
		data = data[room:]
	}
	if len(data) > 0 && a.spillBytes(data) != nil {
		a.overflow = true
	}
}

func (a *accumulator) spillBytes(data []byte) error {
	if len(data) == 0 {
		return nil
	}
	cost := int64(len(data))
	if !agent.ReserveActiveSpill(a.threadID, cost) {
		return rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	written, err := a.writeSpill(data)
	a.size += int64(written)
	if err != nil {
		agent.ReleaseActiveSpill(a.threadID, cost-int64(written))
		return err
	}
	return nil
}

func (a *accumulator) writeSpill(data []byte) (int, error) {
	if a.spill == nil {
		file, err := os.CreateTemp("", "dalgona-active-*")
		if err != nil {
			return 0, fmt.Errorf("create active spill: %w", err)
		}
		a.spill = file
		a.path = file.Name()
	}
	n, err := a.spill.WriteAt(data, a.size-int64(len(a.memory)))
	if err != nil {
		return n, fmt.Errorf("write active spill: %w", err)
	}
	return n, nil
}

func (a *accumulator) freeze() (frozenContent, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.overflow {
		return frozenContent{}, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	a.pins++
	return frozenContent{acc: a, length: a.size}, nil
}

func (a *accumulator) finish() {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.ended = true
	a.releaseLocked()
}

func (f frozenContent) release() {
	f.acc.mu.Lock()
	defer f.acc.mu.Unlock()
	f.acc.pins--
	f.acc.releaseLocked()
}

func (a *accumulator) releaseLocked() {
	if !a.ended || a.pins > 0 || a.path == "" {
		return
	}
	spilled := a.size - int64(len(a.memory))
	if a.spill != nil {
		_ = a.spill.Close()
		a.spill = nil
	}
	_ = os.Remove(a.path)
	a.path = ""
	agent.ReleaseActiveSpill(a.threadID, spilled)
}

func (f frozenContent) read(offset, limit int64) ([]byte, error) {
	f.acc.mu.Lock()
	defer f.acc.mu.Unlock()
	end := min(offset+limit, f.length)
	out := make([]byte, 0, max(end-offset, 0))
	memory := int64(len(f.acc.memory))
	if offset < memory {
		out = append(out, f.acc.memory[offset:min(end, memory)]...)
	}
	if end > memory {
		start := max(offset, memory)
		count := len(out)
		out = out[:count+int(end-start)]
		if err := f.acc.readSpill(out[count:], start-memory); err != nil {
			return nil, err
		}
	}
	return out, nil
}

func (a *accumulator) readSpill(p []byte, offset int64) error {
	if a.spill != nil {
		_, err := a.spill.ReadAt(p, offset)
		return err
	}
	file, err := os.Open(a.path)
	if err != nil {
		return fmt.Errorf("open live spill: %w", err)
	}
	_, err = file.ReadAt(p, offset)
	return errors.Join(err, file.Close())
}

func (s *server) nativeItem(w *watch, method, turnID string, it *rpc.Item, at int64) {
	switch method {
	case methodItemStarted:
		w.live.begin(it, turnID)
	case methodItemCompleted:
		w.live.end(it.ID)
	}
	native := s.native.Load()
	threadID := w.thread.ID()
	var issued []*liveRef
	header, err := itemHeader(*it, w.tracker.blobs[it.ID], func(field itemField) (rpc.ContentReference, error) {
		ref := &liveRef{
			threadID: threadID, turnID: turnID, itemID: it.ID, field: field.name, head: w.head,
			blob: field.blob, spill: field.spill, total: field.total(), digest: field.digest(),
		}
		key, err := native.contents.register(ref)
		if err != nil {
			return rpc.ContentReference{}, err
		}
		issued = append(issued, ref)
		return fieldReference(rpc.ContentSource{Kind: sourceLive, StreamID: native.streamID}, key, field)
	})
	var params jsontext.Value
	if err == nil {
		params, err = itemNotificationParams(header, method, threadID, turnID, at)
	}
	if err != nil {
		native.contents.withdraw(issued)
		s.log.Warn("rpc: project native item", "thread", threadID, "item", it.ID, "error", err)
		s.closeStream(err)
		return
	}
	ordinal, err := s.conn.Publish(threadID, method, params)
	if err != nil {
		native.contents.withdraw(issued)
		s.log.Debug("rpc: drop native item", "method", method, "error", err)
		return
	}
	native.contents.bind(issued, ordinal)
}

func itemNotificationParams(header rpc.ItemHeader, method, threadID, turnID string, at int64) (jsontext.Value, error) {
	raw, err := json.Marshal(header)
	if err != nil {
		return nil, err
	}
	timeKey := "startedAtMs"
	if method == methodItemCompleted {
		timeKey = "completedAtMs"
	}
	return rpc.Marshal(nativeItemNotification{threadID: threadID, turnID: turnID, timeKey: timeKey, at: at, header: raw})
}

func (s *server) nativeNotify(w *watch, method string, note rpc.Encoder) {
	switch method {
	case "item/agentMessage/delta", "item/commandExecution/outputDelta":
		if delta, ok := note.(rpc.AgentMessageDeltaNotification); ok {
			w.live.append(delta.ItemID, delta.Delta)
		}
	case "item/reasoning/textDelta":
		if delta, ok := note.(rpc.ReasoningTextDeltaNotification); ok {
			w.live.append(delta.ItemID, delta.Delta)
		}
	}
	s.send(method, note)
}

func (s *server) closeStream(cause error) {
	if err := s.conn.Close(); err != nil {
		s.log.Debug("rpc: close native stream", "cause", cause, "error", err)
	}
}

type nativeItemNotification struct {
	threadID string
	turnID   string
	timeKey  string
	at       int64
	header   jsontext.Value
}

func (n nativeItemNotification) Encode(o *rpc.Writer) {
	o.Open()
	o.Str("threadId", n.threadID)
	o.Str("turnId", n.turnID)
	o.Int(n.timeKey, n.at)
	o.Raw("item", n.header)
	o.Close()
}

func (native *nativeSession) spillEvent(threadID string, ordinal uint64, params jsontext.Value) (rpc.ContentReference, error) {
	metadata, fields, err := splitMembers("", params, nil)
	if err != nil {
		return rpc.ContentReference{}, fmt.Errorf("split event params for thread %s: %w", threadID, err)
	}
	if len(fields) >= liveReferenceLimit {
		return rpc.ContentReference{}, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	spill := eventSpiller{source: rpc.ContentSource{Kind: sourceLive, StreamID: native.streamID}, thread: threadID, ordinal: ordinal}
	defer spill.release()
	body := paramsBody{Metadata: metadata, Contents: make([]rpc.ContentReference, 0, len(fields))}
	for _, field := range fields {
		ref, storeErr := spill.store(field)
		if storeErr != nil {
			return rpc.ContentReference{}, storeErr
		}
		body.Contents = append(body.Contents, ref)
	}
	raw, err := json.Marshal(body)
	if err != nil {
		return rpc.ContentReference{}, fmt.Errorf("encode spilled params for thread %s: %w", threadID, err)
	}
	ref, err := spill.store(itemField{name: "params", encoding: encodingJSON, data: raw})
	if err != nil {
		return rpc.ContentReference{}, err
	}
	if err := native.contents.registerSpilled(spill.refs); err != nil {
		return rpc.ContentReference{}, err
	}
	spill.refs = nil
	return ref, nil
}

type eventSpiller struct {
	source  rpc.ContentSource
	thread  string
	ordinal uint64
	refs    []*liveRef
}

func (s *eventSpiller) release() {
	for _, ref := range s.refs {
		ref.releaseContent()
	}
}

func (s *eventSpiller) store(field itemField) (rpc.ContentReference, error) {
	acc := &accumulator{threadID: s.thread, ended: true, pins: 1}
	if err := acc.storeBody(field.data); err != nil {
		acc.pins = 0
		acc.releaseLocked()
		return rpc.ContentReference{}, fmt.Errorf("spill event %s for thread %s: %w", field.name, s.thread, err)
	}
	ref := &liveRef{
		threadID: s.thread, field: field.name, ordinal: s.ordinal, total: acc.size, digest: digestOf(field.data),
		content: frozenContent{acc: acc, length: acc.size},
	}
	ref.prepare()
	s.refs = append(s.refs, ref)
	return rpc.ContentReference{
		Source: s.source, ContentID: ref.key, Field: field.name, Encoding: field.encoding,
		Total: rpc.FormatDecimal(uint64(len(field.data))), Digest: ref.digest,
	}, nil
}

func (a *accumulator) storeBody(data []byte) error {
	err := a.spillBytes(data)
	if a.spill != nil {
		err = errors.Join(err, a.spill.Close())
		a.spill = nil
	}
	return err
}

func (s *server) liveRange(ctx context.Context, ref liveRef, window byteRange) ([]byte, error) {
	if ref.content.acc != nil {
		return ref.content.read(window.offset, window.limit)
	}
	if ref.blob != nil {
		return s.blobRange(ref.blob, window)
	}
	field := itemField{name: ref.field, spill: ref.spill}
	if ref.spill == nil {
		var err error
		if field, err = s.liveField(ctx, ref); err != nil {
			return nil, err
		}
	}
	return s.fieldRange(ctx, ref.threadID, field, window)
}

func (s *server) liveField(ctx context.Context, ref liveRef) (itemField, error) {
	thread, err := s.open(ctx, ref.threadID)
	if err != nil {
		return itemField{}, err
	}
	records, _, err := thread.Records()
	if err != nil {
		return itemField{}, hostError(err, ref.threadID, "")
	}
	if ref.head > uint64(len(records)) {
		return itemField{}, errContentDrift
	}
	it, blobs, ok := s.itemAt(ref.threadID, records[:ref.head], ref.turnID, ref.itemID)
	if !ok {
		return itemField{}, errContentDrift
	}
	field, ok, err := fieldOfItem(it, blobs, ref.field)
	if err != nil || !ok || field.total() != ref.total || field.digest() != ref.digest {
		return itemField{}, errors.Join(errContentDrift, err)
	}
	return field, nil
}

func (s *server) fieldRange(ctx context.Context, threadID string, field itemField, window byteRange) ([]byte, error) {
	switch {
	case field.blob != nil:
		return s.blobRange(field.blob, window)
	case field.spill != nil:
		return s.spillRange(ctx, threadID, field.spill, window)
	}
	return field.data[window.offset:min(window.offset+window.limit, int64(len(field.data)))], nil
}

func (s *server) blobRange(blob *event.BlobRef, window byteRange) ([]byte, error) {
	data, total, err := s.host.BlobRange(blob, window.offset, int(window.limit))
	if err != nil {
		return nil, err
	}
	if total != blob.Bytes {
		return nil, errContentDrift
	}
	return data, nil
}

func (s *server) spillRange(ctx context.Context, threadID string, spill *rpc.OutputSpill, window byteRange) ([]byte, error) {
	if sha, ok := strings.CutPrefix(spill.Path, spillBlobPrefix); ok {
		if sha != spill.SHA256 {
			return nil, errContentDrift
		}
		return s.blobRange(&event.BlobRef{Blob: sha, Bytes: spill.Bytes}, window)
	}
	if window.offset == spill.Bytes {
		return nil, nil
	}
	thread, err := s.open(ctx, threadID)
	if err != nil {
		return nil, err
	}
	data, _, err := thread.ReadSpillExact(ctx, &event.Spill{Path: spill.Path, Bytes: spill.Bytes, SHA256: spill.SHA256}, window.offset, window.limit)
	if errors.Is(err, agent.ErrSpillChanged) {
		return nil, errContentDrift
	}
	if err != nil {
		return nil, err
	}
	return data, nil
}

func (s *server) itemAt(threadID string, records []event.Seqd, turnID, itemID string) (rpc.Item, fieldBlobs, bool) {
	path := event.ActivePath(records)
	spans := turnSpans(path)
	index := slices.IndexFunc(spans, func(span turnSpan) bool { return turnIDAt(path, span) == turnID })
	if index < 0 {
		return rpc.Item{}, nil, false
	}
	tr := s.contentTracker(threadID)
	if created, ok := sessionCreated(records); ok {
		tr.created = created
	}
	turns, blobs := tr.collectSourced(path[spans[index].start:spans[index].end])
	for _, turn := range turns {
		if i := slices.IndexFunc(turn.Items, func(it rpc.Item) bool { return it.ID == itemID }); i >= 0 {
			return turn.Items[i], blobs[itemID], true
		}
	}
	return rpc.Item{}, nil, false
}

func (s *server) contentTracker(threadID string) *tracker {
	tr := s.newTracker(threadID)
	tr.deferred = true
	return tr
}

func rangeChunk(data []byte, offset, total int64, digest string) (rpc.ContentChunk, error) {
	if offset < 0 || total < 0 {
		return rpc.ContentChunk{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return rpc.ContentChunk{
		DataBase64: base64.StdEncoding.EncodeToString(data), Offset: rpc.FormatDecimal(uint64(offset)),
		Next: rpc.FormatDecimal(uint64(offset) + uint64(len(data))), Total: rpc.FormatDecimal(uint64(total)), Digest: digest,
	}, nil
}

func turnIDAt(path []event.Seqd, span turnSpan) string {
	started, _ := path[span.start].Event.(event.TurnStarted)
	return started.TurnID
}

func contentChunk(data []byte, digest string, offset, limit int64) (rpc.ContentChunk, error) {
	if offset < 0 || limit < 0 {
		return rpc.ContentChunk{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	total := int64(len(data))
	start := min(offset, total)
	return rangeChunk(data[start:min(start+limit, total)], start, total, digest)
}

type serverKey struct{}

var errNoConnection = errors.New("rpcserver: the context is not serving a connection")

func ConnectionID(ctx context.Context) string {
	s, ok := ctx.Value(serverKey{}).(*server)
	if !ok {
		return ""
	}
	return s.connection
}

func Notify(ctx context.Context, method string, params jsontext.Value) error {
	s, ok := ctx.Value(serverKey{}).(*server)
	if !ok {
		return errNoConnection
	}
	if err := s.conn.Notify(method, params); err != nil {
		return fmt.Errorf("notify %s: %w", method, err)
	}
	return nil
}

func (s *server) releaseNative(watches []*watch) {
	native := s.native.Load()
	if native == nil {
		return
	}
	native.histories.closeAll()
	native.contents.close()
	for _, w := range watches {
		w.apply.Lock()
		w.live.close()
		w.apply.Unlock()
	}
}
