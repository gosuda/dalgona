package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func freeTextQuestion(itemID string) string {
	return `{"threadId":"t1","turnId":"turn-1","itemId":"` + itemID + `","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Why","question":"Why?","isOther":false,"isSecret":false,"multiSelect":false,"options":null}]}`
}

func choiceQuestion(itemID string) string {
	return `{"threadId":"t1","turnId":"turn-1","itemId":"` + itemID + `","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Store","question":"Which store?","isOther":false,"isSecret":false,"multiSelect":false,` +
		`"options":[{"label":"SQLite","description":"One file."},{"label":"Postgres","description":"A server."}]}]}`
}

func queuedQuestion(requestID, itemID string, answer chan requestAnswer) serverRequestMsg {
	return serverRequestMsg{method: "item/tool/requestUserInput", requestID: requestID, params: []byte(freeTextQuestion(itemID)), answer: answer}
}

func queuedApproval(requestID, itemID string, answer chan requestAnswer) serverRequestMsg {
	return serverRequestMsg{
		method:    "item/commandExecution/requestApproval",
		requestID: requestID,
		params:    []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"` + itemID + `","startedAtMs":1,"command":"go test ./..."}`),
		answer:    answer,
	}
}

func openTypingQuestion(t *testing.T, typed string) (model, chan requestAnswer) {
	t.Helper()
	answer := make(chan requestAnswer, 1)
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, queuedQuestion("req-1", "q-1", answer))
	if len(m.approvals) != 1 || m.approvals[0].card == nil || !m.approvals[0].card.typing {
		t.Fatalf("free-text question did not open in typing mode: %#v", m.approvals)
	}
	m = typeText(t, m, typed)
	if got := m.input.field.value(); got != typed {
		t.Fatalf("field = %q before the second request, want %q", got, typed)
	}
	return m, answer
}

func assertField(t *testing.T, m model, want, when string) {
	t.Helper()
	if got := m.input.field.value(); got != want {
		t.Fatalf("field = %q after %s, want %q", got, when, want)
	}
}

func assertTypedAnswerSent(t *testing.T, m model, answer chan requestAnswer, want string) {
	t.Helper()
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	_, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if got := answerJSON(t, answer); !strings.Contains(got, `"q1":{"answers":["`+want+`"]}`) {
		t.Fatalf("answer = %s, want the typed text %q", got, want)
	}
}

func TestSecondRequestKeepsTypedAnswer(t *testing.T) {
	t.Parallel()
	t.Run("approval joins the queue", func(t *testing.T) {
		t.Parallel()
		m, answer := openTypingQuestion(t, "because")
		m, _ = drive(t, m, queuedApproval("req-2", "cmd-2", make(chan requestAnswer, 1)))
		assertField(t, m, "because", "an approval arrived")
		assertContains(t, m.View().Content, "answer: because")
		assertTypedAnswerSent(t, m, answer, "because")
	})
	t.Run("question joins the queue", func(t *testing.T) {
		t.Parallel()
		m, answer := openTypingQuestion(t, "because")
		m, _ = drive(t, m, queuedQuestion("req-2", "q-2", make(chan requestAnswer, 1)))
		assertField(t, m, "because", "a question arrived")
		assertContains(t, m.View().Content, "answer: because")
		assertTypedAnswerSent(t, m, answer, "because")
	})
	t.Run("API key survives two approvals", func(t *testing.T) {
		t.Parallel()
		m := openAPIKeyField(t, "sk-1234")
		m, _ = drive(t, m, queuedApproval("req-1", "cmd-1", make(chan requestAnswer, 1)))
		assertField(t, m, "sk-1234", "the first approval arrived")
		m, _ = drive(t, m, queuedApproval("req-2", "cmd-2", make(chan requestAnswer, 1)))
		assertField(t, m, "sk-1234", "the second approval arrived")
		m, _ = drive(t, m, press('y', 0, "y"))
		m, _ = drive(t, m, press('y', 0, "y"))
		if m.login == nil || m.login.phase != loginAPIKey {
			t.Fatal("login panel left the API key field")
		}
		assertContains(t, m.View().Content, "API key: "+strings.Repeat("*", 7))
	})
}

func openAPIKeyField(t *testing.T, key string) model {
	t.Helper()
	m := openLoginPanel(t, new(fakeCaller), "openai")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	return typeText(t, m, key)
}

func dismissFirstOfTwoQuestions(t *testing.T, typedBefore bool) model {
	t.Helper()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, queuedQuestion("req-1", "q-1", make(chan requestAnswer, 1)))
	if typedBefore {
		m = typeText(t, m, "draft")
	}
	m, _ = drive(t, m, queuedQuestion("req-2", "q-2", make(chan requestAnswer, 1)))
	if !typedBefore {
		m = typeText(t, m, "draft")
	}
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	return m
}

func TestQueuedQuestionStartsWithEmptyAnswer(t *testing.T) {
	t.Parallel()
	for name, typedBefore := range map[string]bool{"typed before the second request": true, "typed after the second request": false} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			m := dismissFirstOfTwoQuestions(t, typedBefore)
			if len(m.approvals) != 1 || m.approvals[0].itemID != "q-2" || !m.approvals[0].card.typing {
				t.Fatalf("second question is not the open typing card: %#v", m.approvals)
			}
			assertField(t, m, "", "the first question closed")
			if view := m.View().Content; strings.Contains(view, "draft") {
				t.Fatalf("second question shows the text typed on the first:\n%s", view)
			}
		})
	}
}

func TestQueuedQuestionNeverShowsAPIKey(t *testing.T) {
	t.Parallel()
	m := openAPIKeyField(t, "sk-1234")
	m, _ = drive(t, m, queuedApproval("req-1", "cmd-1", make(chan requestAnswer, 1)))
	answer := make(chan requestAnswer, 1)
	m, _ = drive(t, m, queuedQuestion("req-2", "q-2", answer))
	m, _ = drive(t, m, press('y', 0, "y"))
	if len(m.approvals) != 1 || m.approvals[0].card == nil || !m.approvals[0].card.typing {
		t.Fatalf("question is not the open typing card: %#v", m.approvals)
	}
	if view := m.View().Content; strings.Contains(view, "sk-1234") {
		t.Fatalf("question card shows the typed key:\n%s", view)
	}
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	_, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if got := answerJSON(t, answer); strings.Contains(got, "sk-1234") {
		t.Fatalf("answer = %s, want no key", got)
	}
}

func TestChoiceQuestionKeepsTheAPIKey(t *testing.T) {
	t.Parallel()
	m := openAPIKeyField(t, "sk-1234")
	answer := make(chan requestAnswer, 1)
	m, _ = drive(t, m, serverRequestMsg{method: "item/tool/requestUserInput", requestID: "req-1", params: []byte(choiceQuestion("q-1")), answer: answer})
	if len(m.approvals) != 1 || m.approvals[0].card == nil || m.approvals[0].card.typing {
		t.Fatalf("choice question is not the open card without typing: %#v", m.approvals)
	}
	assertField(t, m, "sk-1234", "a choice question arrived")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if len(m.approvals) != 1 || !m.approvals[0].card.sent {
		t.Fatalf("the sent question is not kept for correction: %#v", m.approvals)
	}
	assertField(t, m, "sk-1234", "the choice question was sent")
	m, _ = drive(t, m, notificationMsg{method: "serverRequest/resolved", params: []byte(`{"threadId":"t1","requestId":"req-1","resolution":"answered"}`)})
	if len(m.approvals) != 0 {
		t.Fatalf("the resolved question is still open: %#v", m.approvals)
	}
	assertField(t, m, "sk-1234", "the choice question closed")
	if m.login == nil || m.login.phase != loginAPIKey {
		t.Fatal("login panel left the API key field")
	}
	assertContains(t, m.View().Content, "API key: "+strings.Repeat("*", 7))
}

func TestFreeTextQuestionTakesTheAPIKeyField(t *testing.T) {
	t.Parallel()
	m := openAPIKeyField(t, "sk-1234")
	m, _ = drive(t, m, queuedQuestion("req-1", "q-1", make(chan requestAnswer, 1)))
	if len(m.approvals) != 1 || m.approvals[0].card == nil || !m.approvals[0].card.typing {
		t.Fatalf("free-text question is not the open typing card: %#v", m.approvals)
	}
	assertField(t, m, "", "a free-text question arrived")
	if view := m.View().Content; strings.Contains(view, "sk-1234") {
		t.Fatalf("question card shows the typed key:\n%s", view)
	}
}

func TestClosedQuestionLeavesTheAPIKeyFieldEmpty(t *testing.T) {
	t.Parallel()
	m := openAPIKeyField(t, "sk-1234")
	m, _ = drive(t, m, queuedQuestion("req-1", "q-1", make(chan requestAnswer, 1)))
	m = typeText(t, m, "draft")
	m, _ = drive(t, m, queuedApproval("req-2", "cmd-2", make(chan requestAnswer, 1)))
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if len(m.approvals) != 1 || m.approvals[0].card != nil {
		t.Fatalf("approval is not the open card: %#v", m.approvals)
	}
	assertField(t, m, "", "the question closed")
	m, _ = drive(t, m, press('y', 0, "y"))
	if m.login == nil || m.login.phase != loginAPIKey {
		t.Fatal("login panel left the API key field")
	}
	if view := m.View().Content; strings.Contains(view, "*") {
		t.Fatalf("API key field shows the text of the closed question:\n%s", view)
	}
}
