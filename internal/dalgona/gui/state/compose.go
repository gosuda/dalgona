package state

import (
	"encoding/json/jsontext"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	methodKeywordsMatch = "dalgona/keywords/match"
	keywordTextLimit    = 64 << 10
)

type (
	NewThread struct {
		Window   WindowID
		Endpoint client.EndpointID
		Draft    Draft
	}
	MatchKeywords struct {
		Key  DraftKey
		Text string
	}
	DraftKeywords struct {
		Text    string
		Matches []rpc.KeywordMatch
	}
	threadMeta struct {
		model    string
		approval string
		effort   string
	}
	keywordState struct {
		ticket  uint64
		pending string
		result  DraftKeywords
	}
	keywordCall   struct{ key DraftKey }
	keywordParams struct {
		ThreadID string `json:"threadId"`
		Text     string `json:"text"`
	}
	keywordsNote struct {
		Matches []keywordNote `json:"matches"`
	}
	keywordNote struct {
		Word      string `json:"word"`
		ByteRange struct {
			Start int `json:"start"`
			End   int `json:"end"`
		} `json:"byteRange"`
		Hue     [2]int `json:"hue"`
		Effort  string `json:"effort,omitzero"`
		Summary string `json:"summary"`
	}
)

func (i NewThread) apply(m *machine) {
	key := DraftKey{Endpoint: i.Endpoint, Window: i.Window}
	m.submit(key, i.Draft, &mutation{key: key, method: methodThreadStart, params: rpc.ThreadStartParams{}, payload: i.Draft})
}

func (m *machine) threadStarted(mu *mutation, raw jsontext.Value) {
	ep := m.endpoint(mu.endpoint())
	response, err := decode[rpc.ThreadStartResponse](raw)
	if err != nil || response.Thread.ID == "" {
		m.place(mu.key, mu.payload)
		m.notify(Notice{Endpoint: ep.id, Text: "The new thread did not report its id; find it in the thread list and send the message there"})
		return
	}
	key := ThreadKey{Endpoint: ep.id, Thread: response.Thread.ID}
	m.meta[key] = threadMeta{model: response.Model, approval: response.ApprovalPolicy, effort: response.ReasoningEffort}
	m.started[mu.key.Window] = key
	m.insertRow(ep, response.Thread)
	if ts := m.watch(ep, mu.key.Window, key); ts != nil && ep.live && ts.proj == nil && ts.opening == 0 {
		m.open(ep, ts)
	}
	next := DraftKey{Endpoint: ep.id, Window: mu.key.Window, Thread: key.Thread}
	turn := &mutation{key: next, method: methodTurnStart, params: rpc.TurnStartParams{ThreadID: key.Thread, Input: mu.payload.input()}, payload: mu.payload}
	if !m.admit(ep, turn) {
		m.place(next, mu.payload)
	}
}

func (i MatchKeywords) apply(m *machine) {
	ep := m.endpoint(i.Key.Endpoint)
	if len(i.Text) > keywordTextLimit {
		return
	}
	m.use(ep)
	if !ep.live {
		return
	}
	state, ok := m.keywords[i.Key]
	if !ok {
		state = &keywordState{}
		m.keywords[i.Key] = state
	}
	state.pending = i.Text
	state.ticket = m.call(ep, keywordCall{key: i.Key}, methodKeywordsMatch, keywordParams{ThreadID: i.Key.Thread, Text: i.Text})
}

func (c keywordCall) resume(m *machine, r resultMsg) {
	state, ok := m.keywords[c.key]
	if !ok || state.ticket != r.ticket || r.err != nil {
		return
	}
	state.ticket = 0
	note, err := decode[keywordsNote](r.raw)
	if err != nil {
		return
	}
	matches := make([]rpc.KeywordMatch, 0, len(note.Matches))
	for _, k := range note.Matches {
		matches = append(matches, rpc.KeywordMatch{
			Word: k.Word, ByteRange: rpc.ByteRange{Start: k.ByteRange.Start, End: k.ByteRange.End}, Hue: k.Hue, Effort: k.Effort, Summary: k.Summary,
		})
	}
	state.result = DraftKeywords{Text: state.pending, Matches: matches}
}
