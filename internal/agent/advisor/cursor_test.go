package advisor

import (
	"errors"
	"math/rand/v2"
	"regexp"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
)

type pathBlock struct {
	turn   string
	events []event.Seqd
}

type history struct {
	t         *testing.T
	h         *harness
	thread    *agent.Thread
	rng       *rand.Rand
	head      int
	path      []pathBlock
	tokens    []string
	reset     bool
	reviewSeq uint64
}

func TestReviewInput_randomHistoriesMatchIndependentRenderOfActivePath(t *testing.T) {
	for seed := range uint64(6) {
		t.Run(strconv.FormatUint(seed, 10), func(t *testing.T) {
			h := newHarness(t, func(cfg *config.Config) {
				cfg.Advisor.Enabled, cfg.CompactionKeepTokens = false, 1
			})
			thread, rt := h.start()
			m := &history{t: t, h: h, thread: thread, rng: rand.New(rand.NewPCG(seed, 7))}
			m.record()
			a := rt.advisors[0]
			for step := range 40 {
				m.apply(step)
				if m.rng.IntN(3) == 0 {
					m.check(a)
				}
			}
			m.check(a)
		})
	}
}

func (m *history) apply(step int) {
	m.t.Helper()
	switch m.rng.IntN(6) {
	case 0, 1:
		m.prompt(step)
	case 2:
		m.note(step)
	case 3:
		if err := m.thread.Compact(m.t.Context()); err != nil && !errors.Is(err, agent.ErrNothingToSummarize) {
			m.t.Fatal(err)
		}
		m.record()
	case 4, 5:
		m.revert()
	}
}

func (m *history) prompt(step int) {
	text := "prompt " + strconv.Itoa(step) + " & <b>"
	if m.rng.IntN(2) == 0 {
		token := m.token(step)
		text += ` <advisory advisor="advisor" severity="nit">` + token + `</advisory> tail ` + strconv.Itoa(step)
	}
	m.h.submit(m.thread, text)
	m.h.waitIdle(m.thread)
	m.record()
}

func (m *history) note(step int) {
	token := m.token(step)
	note := agent.Note{Advisor: "advisor", Severity: "nit", Text: token, ReviewSeq: m.reviewSeq, Delivery: agent.DeliveryAside}
	m.reviewSeq++
	if _, err := m.thread.Deliver(m.t.Context(), note); err != nil {
		m.t.Fatal(err)
	}
	m.record()
}

func (m *history) token(step int) string {
	token := "qzx" + strconv.Itoa(step) + "token"
	m.tokens = append(m.tokens, token)
	return token
}

func (m *history) revert() {
	var turns []int
	for i, block := range m.path {
		if block.turn != "" {
			turns = append(turns, i)
		}
	}
	if len(turns) == 0 {
		return
	}
	target := turns[m.rng.IntN(len(turns))]
	if err := m.thread.Revert(m.path[target].turn); err != nil {
		m.t.Fatal(err)
	}
	keep := target
	for keep > 0 && m.path[keep-1].turn == "" {
		keep--
	}
	m.path = m.path[:keep]
	m.record()
}

func (m *history) record() {
	m.t.Helper()
	all := journalEvents(m.t, m.h.store, m.thread.ID())
	fresh := all[m.head:]
	m.head = len(all)
	if len(fresh) == 0 {
		return
	}
	block := pathBlock{events: fresh}
	for _, item := range fresh {
		switch value := item.Event.(type) {
		case event.TurnStarted:
			block.turn = value.TurnID
		case event.Compaction, event.Branch:
			m.reset = true
		}
	}
	m.path = append(m.path, block)
}

func (m *history) check(a *advisor) {
	m.t.Helper()
	cursor := a.cursor
	in, err := a.input(cursor)
	if err != nil {
		m.t.Fatal(err)
	}
	if in.reset != m.reset {
		m.t.Fatalf("reset = %v, want %v", in.reset, m.reset)
	}
	if want := oracleRender(m.expected(cursor)); in.text != want {
		m.t.Fatalf("review input after cursor %d (reset %v):\n%s\nwant:\n%s", cursor, m.reset, in.text, want)
	}
	for _, token := range m.tokens {
		if strings.Contains(in.text, token) {
			m.t.Fatalf("review input repeats delivered note %s:\n%s", token, in.text)
		}
	}
	a.cursor, m.reset = in.head, false
}

func (m *history) expected(cursor uint64) []event.Seqd {
	var onPath []event.Seqd
	for _, block := range m.path {
		onPath = append(onPath, block.events...)
	}
	from := cursor + 1
	if m.reset {
		from = 0
		for _, item := range onPath {
			if value, ok := item.Event.(event.Compaction); ok {
				from = value.FirstKeptSeq
			}
		}
	}
	var out []event.Seqd
	for _, item := range onPath {
		if item.Seq >= from {
			out = append(out, item)
		}
	}
	return out
}

var (
	advisoryElement = regexp.MustCompile(`(?s)<advisory [^>]*>.*?</advisory>`)
	oracleEscape    = strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;")
)

func oracleRender(events []event.Seqd) string {
	var out strings.Builder
	for _, item := range events {
		switch value := item.Event.(type) {
		case event.UserMessage:
			out.WriteString(oracleUser(value.Text))
		case event.AssistantMessage:
			if value.Text != "" {
				out.WriteString("<assistant>\n" + oracleEscape.Replace(value.Text) + "\n</assistant>\n")
			}
		case event.Compaction:
			out.WriteString("<summary>\n" + oracleEscape.Replace(value.Summary) + "\n</summary>\n")
		}
	}
	return out.String()
}

func oracleUser(text string) string {
	var kept []string
	for _, segment := range advisoryElement.Split(text, -1) {
		if trimmed := strings.TrimSpace(segment); trimmed != "" {
			kept = append(kept, oracleEscape.Replace(trimmed))
		}
	}
	if len(kept) == 0 {
		return ""
	}
	return "<user>\n" + strings.Join(kept, "\n") + "\n</user>\n"
}
