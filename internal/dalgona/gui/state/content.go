package state

import (
	"errors"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	methodImage = "dalgona/item/image/read"
	imageChunk  = 1 << 20
	pageBudget  = 16 << 20
)

var errUnknownContent = errors.New("the item no longer references this content; refresh the thread")

type (
	ContentKey struct {
		Item      ItemKey
		ContentID string
		Offset    uint64
	}
	ContentPage struct {
		Data    []byte
		Next    uint64
		Total   uint64
		Loading bool
		Err     error
	}
	ImageKey struct {
		Item   ItemKey
		Index  int
		Offset uint64
	}
	ImagePage struct {
		Data    []byte
		Next    uint64
		Total   uint64
		Mime    string
		Digest  string
		Loading bool
		Err     error
	}
	ReadContent struct {
		Thread    ThreadKey
		Item      ItemKey
		ContentID string
		Offset    uint64
	}
	ReadImage struct {
		Thread ThreadKey
		Item   ItemKey
		Index  int
		Offset uint64
	}
	ReleaseContentPage struct {
		Thread ThreadKey
		Key    ContentKey
	}
	ReleaseImagePage struct {
		Thread ThreadKey
		Key    ImageKey
	}
)

type (
	pageStore struct {
		contents map[ContentKey]*pageSlot
		images   map[ImageKey]*pageSlot
		order    []*pageSlot
		bytes    int
	}
	pageSlot struct {
		content ContentKey
		image   ImageKey
		isImage bool
		ticket  uint64
		data    []byte
		next    uint64
		total   uint64
		mime    string
		digest  string
		err     error
	}
	pageRead struct {
		thread  ThreadKey
		link    client.Link
		slot    *pageSlot
		summary rpc.ContentSummary
	}
	imageParams struct {
		ThreadID   string `json:"threadId"`
		TurnID     string `json:"turnId"`
		ItemID     string `json:"itemId"`
		ImageIndex int    `json:"imageIndex"`
		Offset     string `json:"offset"`
		Limit      int    `json:"limit"`
	}
	imageChunkNote struct {
		MimeType   string `json:"mimeType"`
		Digest     string `json:"digest"`
		Total      string `json:"total"`
		Offset     string `json:"offset"`
		Next       string `json:"next"`
		DataBase64 string `json:"dataBase64"`
	}
)

func (s *pageStore) slot(found *pageSlot, create func() *pageSlot) *pageSlot {
	if found != nil {
		return found
	}
	slot := create()
	s.order = append(s.order, slot)
	return slot
}

func (s *pageStore) contentSlot(key ContentKey) *pageSlot {
	if s.contents == nil {
		s.contents = map[ContentKey]*pageSlot{}
	}
	return s.slot(s.contents[key], func() *pageSlot {
		slot := &pageSlot{content: key}
		s.contents[key] = slot
		return slot
	})
}

func (s *pageStore) imageSlot(key ImageKey) *pageSlot {
	if s.images == nil {
		s.images = map[ImageKey]*pageSlot{}
	}
	return s.slot(s.images[key], func() *pageSlot {
		slot := &pageSlot{image: key, isImage: true}
		s.images[key] = slot
		return slot
	})
}

func (s *pageStore) drop(slot *pageSlot) {
	if slot.isImage {
		delete(s.images, slot.image)
	} else {
		delete(s.contents, slot.content)
	}
	s.bytes -= len(slot.data)
	s.order = slices.DeleteFunc(s.order, func(x *pageSlot) bool { return x == slot })
}

func (s *pageStore) admit(fresh *pageSlot) {
	s.bytes += len(fresh.data)
	for _, slot := range slices.Clone(s.order) {
		if s.bytes <= pageBudget {
			return
		}
		if slot != fresh && slot.ticket == 0 && slot.err == nil {
			s.drop(slot)
		}
	}
}

func (slot *pageSlot) settled() bool { return slot.ticket != 0 || slot.err == nil && slot.data != nil }

func (p *projection) reference(item ItemKey, contentID string) (rpc.ContentReference, bool) {
	t, ok := p.turnByID[item.Turn]
	if !ok || t.byID[item.Item] == nil {
		return rpc.ContentReference{}, false
	}
	for _, ref := range t.byID[item.Item].header().Contents {
		if ref.ContentID == contentID {
			return ref, true
		}
	}
	return rpc.ContentReference{}, false
}

func (i ReadContent) apply(m *machine) {
	ep, ts := m.thread(i.Thread)
	if ts == nil || ts.proj == nil || !ep.live {
		return
	}
	slot := ts.pages.contentSlot(ContentKey{Item: i.Item, ContentID: i.ContentID, Offset: i.Offset})
	if slot.settled() {
		return
	}
	m.touch(ts)
	ref, ok := ts.proj.reference(i.Item, i.ContentID)
	if !ok {
		slot.err = errUnknownContent
		return
	}
	slot.err = nil
	params := contentParams{Source: ref.Source, ContentID: ref.ContentID, Offset: rpc.FormatDecimal(i.Offset), Limit: readChunk}
	summary := rpc.ContentSummary{Total: ref.Total, Digest: ref.Digest}
	slot.ticket = m.call(ep, pageRead{thread: i.Thread, link: ep.link, slot: slot, summary: summary}, methodContent, params)
}

func (i ReadImage) apply(m *machine) {
	ep, ts := m.thread(i.Thread)
	if ts == nil || ts.proj == nil || !ep.live {
		return
	}
	slot := ts.pages.imageSlot(ImageKey{Item: i.Item, Index: i.Index, Offset: i.Offset})
	if slot.settled() {
		return
	}
	m.touch(ts)
	slot.err = nil
	params := imageParams{ThreadID: i.Thread.Thread, TurnID: i.Item.Turn, ItemID: i.Item.Item, ImageIndex: i.Index, Offset: rpc.FormatDecimal(i.Offset), Limit: imageChunk}
	slot.ticket = m.call(ep, pageRead{thread: i.Thread, link: ep.link, slot: slot}, methodImage, params)
}

func (c pageRead) resume(m *machine, r resultMsg) {
	_, ts, ok := m.current(c.thread, c.link)
	if !ok || c.slot.ticket != r.ticket || !slices.Contains(ts.pages.order, c.slot) {
		return
	}
	c.slot.ticket = 0
	m.touch(ts)
	if c.slot.isImage {
		c.slot.err = c.slot.fillImage(r)
	} else {
		c.slot.err = c.slot.fillContent(c.summary, r)
	}
	if c.slot.err == nil {
		ts.pages.admit(c.slot)
	}
}

func (slot *pageSlot) fillContent(summary rpc.ContentSummary, r resultMsg) error {
	if r.err != nil {
		return r.err
	}
	chunk, err := decode[rpc.ContentChunk](r.raw)
	if err != nil {
		return err
	}
	return slot.fill(chunk, summary, slot.content.Offset)
}

func (slot *pageSlot) fillImage(r resultMsg) error {
	if r.err != nil {
		return r.err
	}
	note, err := decode[imageChunkNote](r.raw)
	if err != nil {
		return err
	}
	slot.mime, slot.digest = note.MimeType, note.Digest
	chunk := rpc.ContentChunk{DataBase64: note.DataBase64, Offset: note.Offset, Next: note.Next, Total: note.Total, Digest: note.Digest}
	return slot.fill(chunk, rpc.ContentSummary{Total: note.Total, Digest: note.Digest}, slot.image.Offset)
}

func (slot *pageSlot) fill(chunk rpc.ContentChunk, summary rpc.ContentSummary, offset uint64) error {
	total, err := rpc.ParseDecimal(summary.Total)
	if err != nil {
		return err
	}
	data, err := chunkBytes(chunk, offset, total, summary)
	if err != nil {
		return err
	}
	slot.data, slot.next, slot.total = data, offset+uint64(len(data)), total
	return nil
}

func (i ReleaseContentPage) apply(m *machine) {
	_, ts := m.thread(i.Thread)
	if ts == nil {
		return
	}
	if slot := ts.pages.contents[i.Key]; slot != nil && slot.ticket == 0 {
		ts.pages.drop(slot)
		m.touch(ts)
	}
}

func (i ReleaseImagePage) apply(m *machine) {
	_, ts := m.thread(i.Thread)
	if ts == nil {
		return
	}
	if slot := ts.pages.images[i.Key]; slot != nil && slot.ticket == 0 {
		ts.pages.drop(slot)
		m.touch(ts)
	}
}

func (s *pageStore) views() (map[ContentKey]ContentPage, map[ImageKey]ImagePage) {
	contents := make(map[ContentKey]ContentPage, len(s.contents))
	for key, slot := range s.contents {
		contents[key] = ContentPage{Data: slot.data, Next: slot.next, Total: slot.total, Loading: slot.ticket != 0, Err: slot.err}
	}
	images := make(map[ImageKey]ImagePage, len(s.images))
	for key, slot := range s.images {
		images[key] = ImagePage{Data: slot.data, Next: slot.next, Total: slot.total, Mime: slot.mime, Digest: slot.digest, Loading: slot.ticket != 0, Err: slot.err}
	}
	return contents, images
}

func (t *ThreadView) Content(k ContentKey) (ContentPage, bool) {
	page, ok := t.contents[k]
	return page, ok
}

func (t *ThreadView) Image(k ImageKey) (ImagePage, bool) {
	page, ok := t.images[k]
	return page, ok
}
