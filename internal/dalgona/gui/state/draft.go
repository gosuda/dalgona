package state

import (
	"cmp"
	"fmt"
	"maps"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	followUpLimit = 32
	followUpBytes = 256 << 10
	noticeLimit   = 5
)

type AttachmentKind uint8

const (
	AttachmentText AttachmentKind = iota
	AttachmentImage
)

type Attachment struct {
	ID   string
	Kind AttachmentKind
	Name string
	Text string
	URL  string
}

type Draft struct {
	Text        string
	Attachments []Attachment
	Revision    uint64
}

type FollowUp struct {
	ID          uint64
	Text        string
	Attachments []Attachment
}

type NoticeTone uint8

const (
	NoticeError NoticeTone = iota
	NoticeStatus
)

type Notice struct {
	ID       uint64
	Endpoint client.EndpointID
	Thread   string
	Text     string
	Tone     NoticeTone
}

func (d Draft) empty() bool { return d.Text == "" && len(d.Attachments) == 0 }

func (d Draft) input() []rpc.InputItem {
	items := make([]rpc.InputItem, 0, 1+len(d.Attachments))
	if d.Text != "" {
		items = append(items, rpc.InputItem{Type: "text", Text: d.Text})
	}
	for _, a := range d.Attachments {
		if a.Kind == AttachmentText {
			items = append(items, rpc.InputItem{Type: "text", Text: a.Text})
		}
	}
	for _, a := range d.Attachments {
		if a.Kind == AttachmentImage {
			items = append(items, rpc.InputItem{Type: "image", URL: a.URL, Name: a.Name})
		}
	}
	return items
}

func (d Draft) textBytes() int {
	n := len(d.Text)
	for _, a := range d.Attachments {
		n += len(a.Text)
	}
	return n
}

func (m *machine) fresh(key DraftKey, d Draft) bool {
	if d.Revision >= m.drafts[key].Revision {
		return true
	}
	m.notify(Notice{Endpoint: key.Endpoint, Thread: key.Thread, Text: "The message changed before it was sent; send it again"})
	return false
}

func (m *machine) consume(key DraftKey, d Draft) {
	m.drafts[key] = Draft{Revision: d.Revision}
	m.dirty = true
}

func (m *machine) keep(key DraftKey, d Draft) {
	if d.Revision >= m.drafts[key].Revision {
		m.drafts[key] = d
		m.dirty = true
	}
}

func (m *machine) place(key DraftKey, d Draft) bool {
	current := m.drafts[key]
	if !current.empty() {
		m.notify(Notice{Endpoint: key.Endpoint, Thread: key.Thread, Text: "Clear the composer before restoring a message into it"})
		return false
	}
	d.Revision = current.Revision + 1
	m.drafts[key] = d
	m.dirty = true
	return true
}

func (m *machine) submit(key DraftKey, d Draft, mu *mutation) {
	if !m.fresh(key, d) || d.empty() {
		return
	}
	if m.admit(m.endpoint(key.Endpoint), mu) {
		m.consume(key, d)
		return
	}
	m.keep(key, d)
}

func (m *machine) activeTurn(key ThreadKey) string {
	_, ts := m.thread(key)
	if ts == nil || ts.proj == nil {
		return ""
	}
	return ts.proj.activeTurn
}

func (m *machine) enqueue(key DraftKey, d Draft) {
	if !m.fresh(key, d) || d.empty() {
		return
	}
	queue := m.queues[key]
	used := d.textBytes()
	for _, f := range queue {
		used += len(f.Text) + Draft{Attachments: f.Attachments}.textBytes()
	}
	if len(queue) >= followUpLimit || used > followUpBytes {
		m.keep(key, d)
		m.notify(Notice{Endpoint: key.Endpoint, Thread: key.Thread, Text: fmt.Sprintf("The follow-up queue is full (%d messages or %d KiB)", followUpLimit, followUpBytes>>10)})
		return
	}
	m.queues[key] = append(queue, FollowUp{ID: m.next(), Text: d.Text, Attachments: d.Attachments})
	m.consume(key, d)
	if ep, ts := m.thread(key.thread()); ts != nil {
		m.progress(ep, ts)
	}
}

func (m *machine) busy(key ThreadKey) bool {
	return slices.ContainsFunc(m.mutations, func(mu *mutation) bool { return mu.key.thread() == key })
}

func (m *machine) progress(ep *endpointState, ts *threadState) {
	if !m.ready(ep, ts) {
		return
	}
	keys := slices.SortedFunc(maps.Keys(m.queues), func(a, b DraftKey) int { return cmp.Compare(a.Window, b.Window) })
	at := slices.IndexFunc(keys, func(k DraftKey) bool { return k.thread() == ts.key && len(m.queues[k]) > 0 })
	if at < 0 {
		return
	}
	key := keys[at]
	head := m.queues[key][0]
	payload := Draft{Text: head.Text, Attachments: head.Attachments}
	mu := &mutation{key: key, method: methodTurnStart, params: rpc.TurnStartParams{ThreadID: key.Thread, Input: payload.input()}, payload: payload}
	if m.admit(ep, mu) {
		m.dropFollowUp(key, head.ID)
	}
}

func (m *machine) ready(ep *endpointState, ts *threadState) bool {
	if ts.proj != nil && ts.proj.finished(ts.starting) {
		ts.starting = ""
	}
	return ep.live && ts.proj != nil && ts.proj.activeTurn == "" && ts.starting == "" && !m.busy(ts.key)
}

func (m *machine) dropFollowUp(key DraftKey, id uint64) (FollowUp, bool) {
	queue := m.queues[key]
	at := slices.IndexFunc(queue, func(f FollowUp) bool { return f.ID == id })
	if at < 0 {
		return FollowUp{}, false
	}
	removed := queue[at]
	queue = slices.Delete(slices.Clone(queue), at, at+1)
	if len(queue) == 0 {
		delete(m.queues, key)
	} else {
		m.queues[key] = queue
	}
	return removed, true
}

func (m *machine) notify(n Notice) {
	m.notices = slices.DeleteFunc(m.notices, func(old Notice) bool { return old.Text == n.Text })
	n.ID = m.next()
	m.notices = append(m.notices, n)
	if over := len(m.notices) - noticeLimit; over > 0 {
		m.notices = slices.Delete(m.notices, 0, over)
	}
}
