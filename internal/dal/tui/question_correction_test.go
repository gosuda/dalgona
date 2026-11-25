package tui

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"slices"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	tea "charm.land/bubbletea/v2"
)

func correctionWarning(threadID, requestID, field string) notificationMsg {
	return notificationMsg{method: "warning", params: []byte(`{"threadId":"` + threadID + `","message":"invalid text: ` + field +
		`: disallowed_control","requestId":"` + requestID + `","reason":"disallowed_control","field":"` + field + `"}`)}
}

func requestResolved(threadID, requestID, resolution string) notificationMsg {
	return notificationMsg{method: "serverRequest/resolved", params: []byte(`{"threadId":"` + threadID + `","requestId":"` + requestID + `","resolution":"` + resolution + `"}`)}
}

func openQuestionWith(t *testing.T, client caller, data string) (model, chan requestAnswer) {
	t.Helper()
	answer := make(chan requestAnswer, 1)
	m := newTestModel(client, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, serverRequestMsg{method: "item/tool/requestUserInput", requestID: "req-1", params: []byte(data), answer: answer})
	return m, answer
}

func submitTyped(t *testing.T, m model) model {
	t.Helper()
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	return m
}

func submitFreeText(t *testing.T, client caller, text string) (model, chan requestAnswer) {
	t.Helper()
	m, answer := openQuestionWith(t, client, freeTextQuestion("call-q"))
	return submitTyped(t, typeText(t, m, text)), answer
}

func questionEntries(m model, header string) []entry {
	var rows []entry
	for _, row := range m.out.entries {
		if strings.HasPrefix(row.text, header+": ") {
			rows = append(rows, row)
		}
	}
	return rows
}

func entryTexts(rows []entry) []string {
	texts := make([]string, 0, len(rows))
	for _, row := range rows {
		texts = append(texts, row.text)
	}
	return texts
}

func assertEntries(t *testing.T, m model, header string, want ...string) []entry {
	t.Helper()
	rows := questionEntries(m, header)
	if got := entryTexts(rows); !slices.Equal(got, want) {
		t.Fatalf("question rows = %q, want %q", got, want)
	}
	return rows
}

func assertSentCard(t *testing.T, m model, when string) {
	t.Helper()
	if len(m.approvals) != 1 || m.approvals[0].card == nil {
		t.Fatalf("question card closed %s, before serverRequest/resolved: %#v", when, m.approvals)
	}
	assertContains(t, stripANSI(strings.Join(m.questionLines(80, 12), "\n")), "Answer sent; waiting for the agent")
}

func TestQuestionSubmitKeepsCardReadOnlyUntilResolved(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m, answer := submitFreeText(t, caller, "because")
	if got := answerJSON(t, answer); !strings.Contains(got, `"q1":{"answers":["because"]}`) {
		t.Fatalf("first answer = %s", got)
	}
	assertSentCard(t, m, "after the first submit")
	assertContains(t, stripANSI(strings.Join(m.questionLines(80, 12), "\n")), "because")
	assertContains(t, liveRowText(m), "Answer sent;")
	if m.notice != "Answer sent; waiting for the agent" {
		t.Fatalf("notice = %q, want the sent notice", m.notice)
	}
	for _, key := range []tea.KeyPressMsg{press('s', 0, "s"), press(tea.KeyEnter, 0, ""), press(tea.KeyEscape, 0, ""), press(tea.KeyTab, 0, ""), press('x', 0, "x")} {
		m, _ = drive(t, m, key)
	}
	assertSentCard(t, m, "after keys on the sent card")
	assertQuestionUnsent(t, answer)
	assertField(t, m, "", "keys on the sent card")
	m, _ = drive(t, m, requestResolved("t1", "req-1", "answered"))
	if len(m.approvals) != 0 {
		t.Fatalf("resolution left the card open: %#v", m.approvals)
	}
	if m.notice != "Question answered" {
		t.Fatalf("notice = %q, want a resolution notice that names no winner", m.notice)
	}
}

func TestQuestionCorrectionWarningRestoresDraftUnderSameRequest(t *testing.T) {
	t.Parallel()
	m, answer := submitFreeText(t, new(fakeCaller), "because")
	_ = answerJSON(t, answer)
	m, _ = drive(t, m, correctionWarning("t1", "req-1", "answers.q1.answers[0]"))
	if len(m.approvals) != 1 || !m.approvals[0].card.typing || m.approvals[0].card.index != 0 {
		t.Fatalf("warning did not reopen typing on the named question: %#v", m.approvals)
	}
	assertField(t, m, "because", "the correction warning")
	if !m.noticeError {
		t.Fatalf("notice %q is not an error", m.notice)
	}
	assertContains(t, m.notice, "Answer not accepted", "answers.q1.answers[0]", "disallowed_control")
	m = submitTyped(t, typeText(t, m, " now"))
	if got := answerJSON(t, answer); !strings.Contains(got, `"q1":{"answers":["because now"]}`) {
		t.Fatalf("corrected answer = %s", got)
	}
	assertSentCard(t, m, "after the corrected submit")
}

func TestQuestionCorrectionWarningOpensTheNamedAnswer(t *testing.T) {
	t.Parallel()
	m, answer := openQuestionWith(t, new(fakeCaller), questionParamsJSON)
	for _, key := range []tea.KeyPressMsg{press('1', 0, "1"), press('1', 0, "1"), press('4', 0, "4")} {
		m, _ = drive(t, m, key)
	}
	m = submitTyped(t, typeText(t, m, "Mongo"))
	assertContains(t, answerJSON(t, answer), `"q2":{"answers":["Search","Mongo"]}`)
	m, _ = drive(t, m, correctionWarning("t1", "req-1", "answers.q2.answers[1]"))
	if len(m.approvals) != 1 {
		t.Fatalf("warning left %d open cards, want the reopened question", len(m.approvals))
	}
	card := m.approvals[0].card
	if card.index != 1 || !card.typing {
		t.Fatalf("warning opened question %d typing %v, want the typed Features answer", card.index, card.typing)
	}
	assertField(t, m, "Mongo", "the correction warning")
	assertContains(t, m.notice, "Answer not accepted", "answers.q2.answers[1]", "disallowed_control")
	assertContains(t, stripANSI(strings.Join(m.questionLines(80, 12), "\n")), "Question 2 of 2 · Features")
}

func TestQuestionWarningWithoutOpenSentRequestChangesNothing(t *testing.T) {
	t.Parallel()
	for name, warning := range map[string]notificationMsg{
		"another request": correctionWarning("t1", "req-2", "answers.q1.answers[0]"),
		"another thread":  correctionWarning("t2", "req-1", "answers.q1.answers[0]"),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			m, answer := submitFreeText(t, new(fakeCaller), "because")
			_ = answerJSON(t, answer)
			m, _ = drive(t, m, warning)
			assertSentCard(t, m, "after an unrelated warning")
			assertField(t, m, "", "an unrelated warning")
		})
	}
	t.Run("resolved request", func(t *testing.T) {
		t.Parallel()
		m, answer := submitFreeText(t, new(fakeCaller), "because")
		_ = answerJSON(t, answer)
		m, _ = drive(t, m, requestResolved("t1", "req-1", "answered"))
		m, _ = drive(t, m, correctionWarning("t1", "req-1", "answers.q1.answers[0]"))
		if len(m.approvals) != 0 {
			t.Fatalf("stale warning reopened the resolved card: %#v", m.approvals)
		}
		assertField(t, m, "", "a stale warning")
		if m.notice != "Question answered" {
			t.Fatalf("stale warning changed the notice to %q", m.notice)
		}
	})
	t.Run("unsent card", func(t *testing.T) {
		t.Parallel()
		m, answer := openQuestionWith(t, new(fakeCaller), questionParamsJSON)
		m, _ = drive(t, m, press('1', 0, "1"))
		m, _ = drive(t, m, correctionWarning("t1", "req-1", "answers.q1.answers[0]"))
		assertQuestionUnsent(t, answer)
		if card := m.approvals[0].card; card.index != 1 || m.noticeError {
			t.Fatalf("warning without a sent answer moved the card to question %d, notice %q", card.index+1, m.notice)
		}
	})
}

func TestQuestionLocalSubmissionEntriesNumberEachAttempt(t *testing.T) {
	t.Parallel()
	m, answer := submitFreeText(t, new(fakeCaller), "because")
	_ = answerJSON(t, answer)
	assertEntries(t, m, "Why", "Why: Local submission #1: because")
	m, _ = drive(t, m, correctionWarning("t1", "req-1", "answers.q1.answers[0]"))
	m = submitTyped(t, typeText(t, m, " now"))
	if got := answerJSON(t, answer); !strings.Contains(got, `"q1":{"answers":["because now"]}`) {
		t.Fatalf("corrected answer = %s", got)
	}
	rows := assertEntries(t, m, "Why", "Why: Local submission #1: because", "Why: Local submission #2: because now")
	if rows[0].key == rows[1].key {
		t.Fatalf("two attempts share the row key %q", rows[0].key)
	}
}

func TestQuestionLocalRejectionConsumesNoAttempt(t *testing.T) {
	t.Parallel()
	m, answer := openQuestion(t, questionParamsJSON)
	m.approvals[0].card.typed[0] = "a\x00b"
	m, _ = drive(t, m, press('s', 0, "s"))
	assertQuestionUnsent(t, answer)
	assertEntries(t, m, "Database")
	m.approvals[0].card.typed[0] = "Mongo"
	m, _ = drive(t, m, press('s', 0, "s"))
	_ = answerJSON(t, answer)
	assertEntries(t, m, "Database", "Database: Local submission #1: Mongo\nFeatures: Local submission #1: skip requested")
}

func TestQuestionTimeoutKeepsItsOwnRow(t *testing.T) {
	t.Parallel()
	m, answer := submitFreeText(t, new(fakeCaller), "because")
	_ = answerJSON(t, answer)
	m, _ = drive(t, m, requestResolved("t1", "req-1", "timedOut"))
	m, _ = drive(t, m, requestResolved("t1", "req-1", "timedOut"))
	rows := assertEntries(t, m, "Why", "Why: Local submission #1: because", "Why: Timed out")
	if rows[0].key == rows[1].key {
		t.Fatalf("timeout row reuses the submission key %q", rows[0].key)
	}
	if m.notice != "Question timed out" {
		t.Fatalf("notice = %q", m.notice)
	}
}

func TestRequestResolvedClosesOnlyTheMatchingThread(t *testing.T) {
	t.Parallel()
	m, _ := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, requestResolved("t2", "req-1", "answered"))
	if len(m.approvals) != 1 {
		t.Fatal("a resolution for another thread closed the card")
	}
	m, _ = drive(t, m, requestResolved("t1", "req-1", "answered"))
	if len(m.approvals) != 0 {
		t.Fatal("the matching resolution left the card open")
	}
	if m.notice != "Question answered in another window" {
		t.Fatalf("notice = %q", m.notice)
	}
}

func TestRunCorrectsRefusedAnswerUnderTheSameRequest(t *testing.T) {
	t.Parallel()
	warmSignalPackage()
	synctest.Test(t, func(t *testing.T) {
		client, server := net.Pipe()
		input, keyboard := io.Pipe()
		defer keyboard.Close()
		ctx, cancel := context.WithTimeoutCause(t.Context(), 5*time.Second, errors.New("tui correction run timed out"))
		defer cancel()
		output := newOutputBuffer()
		runResult := make(chan error, 1)
		go func() {
			runResult <- Run(ctx, openTestMachine(client), Options{Input: input, Output: output, Width: 80, Height: 24})
		}()
		serverResult := make(chan error, 1)
		go func() { serverResult <- serveCorrection(server) }()
		output.waitFor(ctx, t, "Question 1 of 1 · Why")
		writeKeys(t, keyboard, "because\r\r")
		output.waitFor(ctx, t, "Answer not accepted")
		writeKeys(t, keyboard, " now\r\r")
		output.waitFor(ctx, t, "Question answered")
		writeKeys(t, keyboard, "\x03\x03")
		if err := <-runResult; err != nil {
			t.Fatalf("Run returned %v", err)
		}
		if err := <-serverResult; err != nil {
			t.Fatalf("fake server returned %v", err)
		}
		time.Sleep(time.Minute)
		synctest.Wait()
	})
}

func writeKeys(t *testing.T, keyboard io.Writer, keys string) {
	t.Helper()
	if _, err := keyboard.Write([]byte(keys)); err != nil {
		t.Fatalf("write keys %q: %v", keys, err)
	}
}

func serveCorrection(conn net.Conn) error {
	defer conn.Close()
	reader := bufio.NewReader(conn)
	if err := answerStartup(conn, reader); err != nil {
		return err
	}
	threadStart := `{"approvalPolicy":"on-request","approvalsReviewer":"user","cwd":"/work","model":"gpt-test",` +
		`"modelProvider":"openai","sandbox":{"type":"workspaceWrite"},"thread":` + schemaThread + `}`
	if err := answerRequest(conn, reader, "thread/start", threadStart); err != nil {
		return err
	}
	question := []string{
		`{"jsonrpc":"2.0","method":"thread/started","params":{"thread":` + schemaThread + `}}`,
		`{"jsonrpc":"2.0","method":"turn/started","params":{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}}`,
		`{"jsonrpc":"2.0","id":"ask-1","method":"item/tool/requestUserInput","params":` + freeTextQuestion("call-q") + `}`,
	}
	if err := writeMessages(conn, question); err != nil {
		return err
	}
	if err := readAnswer(reader, `"answers":["because"]`); err != nil {
		return err
	}
	warning := `{"jsonrpc":"2.0","method":"warning","params":{"threadId":"t1","message":"invalid text: answers.q1.answers[0]: disallowed_control",` +
		`"requestId":"ask-1","reason":"disallowed_control","field":"answers.q1.answers[0]"}}`
	if err := writeServerLine(conn, warning); err != nil {
		return err
	}
	if err := readAnswer(reader, `"answers":["because now"]`); err != nil {
		return err
	}
	resolved := `{"jsonrpc":"2.0","method":"serverRequest/resolved","params":{"threadId":"t1","requestId":"ask-1","resolution":"answered"}}`
	if err := writeServerLine(conn, resolved); err != nil {
		return err
	}
	if _, err := io.Copy(io.Discard, reader); err != nil {
		return fmt.Errorf("drain client after the question: %w", err)
	}
	return nil
}

func readAnswer(reader *bufio.Reader, want string) error {
	line, err := reader.ReadString('\n')
	if err != nil {
		return fmt.Errorf("read answer: %w", err)
	}
	if !strings.Contains(line, `"id":"ask-1"`) || !strings.Contains(line, want) || strings.Contains(line, `"error"`) {
		return fmt.Errorf("answer = %s, want %s under id ask-1", line, want)
	}
	return nil
}
