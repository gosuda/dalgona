package tui

import (
	"cmp"
	"slices"
	"strings"
)

const maxHistory = 200

type historyEntry struct {
	turnID string
	text   string
}

type history struct {
	entries  []historyEntry
	index    int
	draft    string
	awaiting bool
}

func (h *history) add(text, turnID string) {
	text = strings.TrimSpace(sanitizeText(text))
	if text == "" || strings.Fields(text)[0] == "/login" {
		return
	}
	h.awaiting = turnID == ""
	if n := len(h.entries); n > 0 && h.entries[n-1].text == text {
		h.entries[n-1].turnID = cmp.Or(turnID, h.entries[n-1].turnID)
		return
	}
	h.entries = append(h.entries, historyEntry{turnID: turnID, text: text})
	if len(h.entries) > maxHistory {
		h.entries = slices.Delete(h.entries, 0, len(h.entries)-maxHistory)
	}
	h.index = len(h.entries)
}

func (h *history) startTurn(turnID string) {
	if n := len(h.entries); n > 0 && h.awaiting {
		h.entries[n-1].turnID = turnID
	}
	h.awaiting = false
}

func (h *history) older(draft string) (string, bool) {
	if h.index == 0 {
		return "", false
	}
	if h.index == len(h.entries) {
		h.draft = draft
	}
	h.index--
	return h.entries[h.index].text, true
}

func (h *history) newer() (string, bool) {
	if h.index >= len(h.entries) {
		return "", false
	}
	h.index++
	if h.index == len(h.entries) {
		return h.draft, true
	}
	return h.entries[h.index].text, true
}
