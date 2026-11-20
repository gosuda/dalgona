package app

import (
	"bytes"
	"encoding/json/v2"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/llmstub"
)

const editJudgeKey = "edit-judge-key"

const editJudgeScript = `{"models":[{"id":"qwen-test"},{"id":"qwen-small"}],"turns":[
{"id":"t-judge","when":{"any_text":"\"off_task\""},"steps":[{"text":"{\"sound\":0.02,\"lost_code\":0.02,\"placeholder\":0.02,\"off_task\":0.02}"},{"finish":"stop"}]},
{"id":"t-judge-turn","when":{"any_text":"\"incomplete\""},"steps":[{"text":"{\"sound\":0.02,\"incomplete\":0.02}"},{"finish":"stop"}]},
{"id":"t-edit","when":{"user":"edit notes"},"steps":[{"tool_call":{"id":"call-edit","name":"patch","arguments":{"changes":[{"path":"notes.md","old":"old line","new":"new line"}]}}},{"finish":"tool_calls"}]},
{"id":"t-edit-2","when":{"tool_result":"call-edit"},"steps":[{"text":"Edited notes.md."},{"finish":"stop"}]}]}`

type lockedBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *lockedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *lockedBuffer) lines() [][]byte {
	b.mu.Lock()
	defer b.mu.Unlock()
	return bytes.Split(bytes.TrimSpace(b.buf.Bytes()), []byte("\n"))
}

func (b *lockedBuffer) turns(t *testing.T) []string {
	t.Helper()
	var turns []string
	for _, line := range b.lines() {
		if len(line) == 0 {
			continue
		}
		var entry struct {
			Turn string `json:"turn"`
		}
		if err := json.Unmarshal(line, &entry); err != nil {
			t.Fatalf("decode stub record %s: %v", line, err)
		}
		turns = append(turns, entry.Turn)
	}
	return turns
}

type editJudgeHost struct {
	host   *agent.Host
	cwd    string
	record *lockedBuffer
}

func newEditJudgeHost(t *testing.T, product config.Product) editJudgeHost {
	t.Helper()
	clearProviderKeys(t)
	script, err := llmstub.Parse("edit-judge", []byte(editJudgeScript))
	if err != nil {
		t.Fatal(err)
	}
	record := &lockedBuffer{}
	server := httptest.NewServer(llmstub.NewHandler(script, llmstub.Options{Key: editJudgeKey, Record: record}))
	t.Cleanup(server.Close)
	t.Setenv("EDIT_JUDGE_KEY", editJudgeKey)
	home, cwd := t.TempDir(), t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	t.Setenv("DAL_HOME", home)
	writeEditJudgeFiles(t, home, cwd, server.URL)
	cfg, err := config.Load(product, cwd, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	cfg.EditReview.Enabled = true
	host, closeHost, err := NewHost(t.Context(), Options{Product: product, Cwd: cwd, Config: &cfg})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := closeHost(); closeErr != nil {
			t.Errorf("close host: %v", closeErr)
		}
	})
	return editJudgeHost{host: host, cwd: cwd, record: record}
}

func writeEditJudgeFiles(t *testing.T, home, cwd, serverURL string) {
	t.Helper()
	body := "edit_style = \"replace\"\n\n[providers.local]\nbase_url = \"" + serverURL + "/v1\"\napi_key_env = \"EDIT_JUDGE_KEY\"\n\n[approval_decider]\ndecider = \"llm:local/qwen-small\"\n"
	if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(cwd, "notes.md"), []byte("# Notes\nold line\nend\n"), 0o600); err != nil {
		t.Fatal(err)
	}
}

func (h editJudgeHost) editNotes(t *testing.T) []event.Seqd {
	t.Helper()
	thread, err := h.host.Start(t.Context(), agent.ThreadSpec{Cwd: h.cwd, Model: "local/qwen-test", Approval: config.ApprovalYolo})
	if err != nil {
		t.Fatal(err)
	}
	updates := thread.Subscribe()
	t.Cleanup(func() { thread.Unsubscribe(updates) })
	if err = thread.Submit(t.Context(), agent.Input{Text: "edit notes"}); err != nil {
		t.Fatal(err)
	}
	awaitTurnCompleted(t, updates)
	records, _, err := thread.Records()
	if err != nil {
		t.Fatal(err)
	}
	return records
}

func awaitTurnCompleted(t *testing.T, updates <-chan agent.Update) {
	t.Helper()
	timeout := time.After(time.Minute)
	for {
		select {
		case update := <-updates:
			if _, done := update.Event.(event.TurnCompleted); done {
				return
			}
		case <-timeout:
			t.Fatal("turn did not complete within a minute")
		}
	}
}

func judgedEvents(records []event.Seqd) (tools []event.ToolJudged, turns int) {
	for _, item := range records {
		switch judged := item.Event.(type) {
		case event.ToolJudged:
			tools = append(tools, judged)
		case event.TurnJudged:
			turns++
		}
	}
	return tools, turns
}

func requireEditJudged(t *testing.T, h editJudgeHost, product config.Product, rule string, want int) {
	t.Helper()
	tools, turns := judgedEvents(h.editNotes(t))
	if len(tools) != want || turns != want {
		t.Fatalf("%s: tool.judged = %d, turn.judged = %d, want %d of each (%s)", product, len(tools), turns, want, rule)
	}
	for _, judged := range tools {
		if judged.By != "judge:local/qwen-small" {
			t.Fatalf("%s: tool.judged by = %q, want judge:local/qwen-small", product, judged.By)
		}
	}
	asked := slices.ContainsFunc(h.record.turns(t), func(turn string) bool { return strings.HasPrefix(turn, "t-judge") })
	if asked != (want > 0) {
		t.Fatalf("%s: judge asked = %v, want %v (%s)", product, asked, want > 0, rule)
	}
}

func TestEditJudgeOnlyInDalgona(t *testing.T) {
	for _, tc := range []struct {
		product config.Product
		rule    string
		judged  int
	}{
		{config.ProductDalgona, "The edit review judges the edit calls of every thread of a dalgona host when `[edit_review] enabled` is true.", 1},
		{config.ProductDal, "A missing judge factory MUST mean that review capability is absent.", 0},
	} {
		t.Run(string(tc.product), func(t *testing.T) {
			requireEditJudged(t, newEditJudgeHost(t, tc.product), tc.product, tc.rule, tc.judged)
		})
	}
}
