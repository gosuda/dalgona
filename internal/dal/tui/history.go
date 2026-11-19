package tui

import (
	"cmp"
	"slices"
	"strings"
)

const maxHistory = 200

type historyEntry struct {
	id     uint64
	turnID string
	text   string
}

type history struct {
	entries    []historyEntry
	lastID     uint64
	revision   uint64
	index      int
	draft      editor
	awaitingID uint64
}

func awaitedID(id uint64, turnID string) uint64 {
	if turnID != "" {
		return 0
	}
	return id
}

func (h *history) add(text, turnID string) {
	text = strings.TrimSpace(sanitizeText(text))
	if text == "" || strings.Fields(text)[0] == "/login" {
		return
	}
	if n := len(h.entries); n > 0 && h.entries[n-1].text == text {
		h.entries[n-1].turnID = cmp.Or(turnID, h.entries[n-1].turnID)
		h.awaitingID = awaitedID(h.entries[n-1].id, turnID)
		return
	}
	h.lastID++
	h.revision++
	h.entries = append(h.entries, historyEntry{id: h.lastID, turnID: turnID, text: text})
	h.awaitingID = awaitedID(h.lastID, turnID)
	if len(h.entries) > maxHistory {
		h.entries = slices.Delete(h.entries, 0, len(h.entries)-maxHistory)
	}
	h.index = len(h.entries)
}

func (h *history) startTurn(turnID string) {
	if index, ok := h.indexOf(h.awaitingID); ok {
		h.entries[index].turnID = turnID
	}
	h.awaitingID = 0
}

func (h *history) allEntries() []historyEntry {
	return slices.Clone(h.entries)
}

func (h *history) indexOf(id uint64) (int, bool) {
	index := slices.IndexFunc(h.entries, func(entry historyEntry) bool { return entry.id == id })
	return index, index >= 0
}

func (h *history) remove(index int) bool {
	if index < 0 || index >= len(h.entries) {
		return false
	}
	h.entries = slices.Delete(h.entries, index, index+1)
	h.index = len(h.entries)
	h.revision++
	return true
}

func (h *history) load(id uint64) (string, bool) {
	index, ok := h.indexOf(id)
	if !ok {
		return "", false
	}
	h.index = len(h.entries)
	return h.entries[index].text, true
}

func (h *history) older(into *editor) bool {
	if h.index == 0 {
		return false
	}
	if h.index == len(h.entries) {
		h.draft = editor{text: into.text, chips: into.chips}
	}
	h.index--
	into.SetValue(h.entries[h.index].text)
	return true
}

func (h *history) newer(into *editor) bool {
	if h.index >= len(h.entries) {
		return false
	}
	h.index++
	if h.index == len(h.entries) {
		into.text, into.chips = slices.Clone(h.draft.text), slices.Clone(h.draft.chips)
		into.cursor = len(into.text)
		return true
	}
	into.SetValue(h.entries[h.index].text)
	return true
}
