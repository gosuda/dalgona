package tui

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"reflect"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const questionParamsJSON = `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
	`{"id":"q1","header":"Database","question":"Which database?","isOther":true,"isSecret":false,"multiSelect":false,` +
	`"options":[{"label":"SQLite","description":"One file."},{"label":"Postgres","description":"A server.","preview":"image: postgres"}]},` +
	`{"id":"q2","header":"Features","question":"Which features?","isOther":true,"isSecret":false,"multiSelect":true,` +
	`"options":[{"label":"Search","description":"Full text."},{"label":"Export","description":"CSV."},{"label":"Sync","description":"Two-way."}]}]}`

func questionRequest(answer chan requestAnswer, data string) serverRequestMsg {
	return serverRequestMsg{method: "item/tool/requestUserInput", requestID: "req-1", params: jsontext.Value(data), answer: answer}
}

func openQuestion(t *testing.T, data string) (model, chan requestAnswer) {
	t.Helper()
	answer := make(chan requestAnswer, 1)
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, questionRequest(answer, data))
	if len(m.approvals) != 1 || m.approvals[0].card == nil {
		t.Fatalf("question was not queued: %#v", m.approvals)
	}
	return m, answer
}

func answerJSON(t *testing.T, answer chan requestAnswer) string {
	t.Helper()
	select {
	case got := <-answer:
		data, err := json.Marshal(got.value)
		if err != nil {
			t.Fatal(err)
		}
		return string(data)
	default:
		t.Fatal("no answer was sent")
	}
	return ""
}

func TestQuestionCardSingleSelect(t *testing.T) {
	t.Parallel()
	single := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Database","question":"Which database?","isOther":true,"isSecret":false,"multiSelect":false,` +
		`"options":[{"label":"SQLite","description":"One file."},{"label":"Postgres","description":"A server."}]}]}`
	m, answer := openQuestion(t, single)
	m, _ = drive(t, m, press('2', 0, "2"))
	assertQuestionUnsent(t, answer)
	if text := stripANSI(strings.Join(m.questionLines(80, 12), "\n")); !strings.Contains(text, "Review · 1 question") {
		t.Fatalf("last answer did not open Review: %s", text)
	}
	_, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if got := answerJSON(t, answer); !strings.Contains(got, `"q1":{"answers":["Postgres"]}`) {
		t.Fatalf("answer = %s", got)
	}
}

func TestQuestionCardMultiSelectKeepsOptionOrder(t *testing.T) {
	t.Parallel()
	m, answer := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, press('1', 0, "1"))
	m, _ = drive(t, m, press('3', 0, "3"))
	m, _ = drive(t, m, press('3', 0, "3"))
	m, _ = drive(t, m, press('1', 0, "1"))
	m, _ = drive(t, m, press('3', 0, "3"))
	m, _ = drive(t, m, press('s', 0, "s"))
	_ = m
	got := answerJSON(t, answer)
	if !strings.Contains(got, `"q2":{"answers":["Search","Sync"]}`) {
		t.Fatalf("answer = %s", got)
	}
}

func TestQuestionCardOtherText(t *testing.T) {
	t.Parallel()
	m, answer := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, press('3', 0, "3"))
	for _, r := range "Mongo" {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, press('s', 0, "s"))
	_ = m
	if got := answerJSON(t, answer); !strings.Contains(got, `"q1":{"answers":["Mongo"]}`) {
		t.Fatalf("answer = %s", got)
	}
}

func TestQuestionCardSubmitNowSkips(t *testing.T) {
	t.Parallel()
	m, answer := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, press('s', 0, "s"))
	_ = m
	if got := answerJSON(t, answer); !strings.Contains(got, `"q1":{"answers":[]}`) {
		t.Fatalf("answer = %s", got)
	}
}

func TestQuestionCardEscCancelsWithEmptyAnswers(t *testing.T) {
	t.Parallel()
	m, answer := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	_ = m
	if got := answerJSON(t, answer); got != `{"answers":{}}` {
		t.Fatalf("answer = %s", got)
	}
}

func TestQuestionCardClosedByResolvedNotification(t *testing.T) {
	t.Parallel()
	m, _ := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, notificationMsg{method: "serverRequest/resolved", params: []byte(`{"threadId":"t1","requestId":"req-1","resolution":"answered"}`)})
	if len(m.approvals) != 0 {
		t.Fatal("resolved question stayed open")
	}
	if m.notice != "Question answered in another window" {
		t.Fatalf("notice = %q", m.notice)
	}
}

func TestQuestionCardClosedByTimedOutNotification(t *testing.T) {
	t.Parallel()
	m, _ := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, notificationMsg{method: "serverRequest/resolved", params: []byte(`{"threadId":"t1","requestId":"req-1","resolution":"timedOut"}`)})
	if len(m.approvals) != 0 {
		t.Fatal("timed-out question stayed open")
	}
	if m.notice != "Question timed out" {
		t.Fatalf("notice = %q", m.notice)
	}
	rows := m.out.entries
	if len(rows) != 1 {
		t.Fatalf("timed-out question printed %d rows, want 1", len(rows))
	}
	const key = `question-status:"t1":"turn-1":"call-q":"req-1":timedOut`
	if rows[0].key != key {
		t.Fatalf("row key %q, want %s", rows[0].key, key)
	}
	if got := rows[0].text; got != "Database: Timed out\nFeatures: Timed out" {
		t.Fatalf("row text %q", got)
	}
	m, _ = drive(t, m, notificationMsg{method: "serverRequest/resolved", params: []byte(`{"threadId":"t1","requestId":"req-1","resolution":"timedOut"}`)})
	m.commit(key, []string{"again"})
	if len(m.out.entries) != 1 {
		t.Fatal("a duplicate resolution or commit printed another status row")
	}
}

func TestQuestionAnsweredResolutionWritesNoRow(t *testing.T) {
	t.Parallel()
	m, _ := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 120, Height: 24})
	m, _ = drive(t, m, notificationMsg{method: "serverRequest/resolved", params: []byte(`{"threadId":"t1","requestId":"req-1","resolution":"answered"}`)})
	if len(m.out.entries) != 0 {
		t.Fatalf("answered resolution wrote %d rows, want 0", len(m.out.entries))
	}
}

func TestQuestionCardFitsWidthAndHeight(t *testing.T) {
	t.Parallel()
	preview := strings.Repeat(`row\n`, 39) + "row"
	data := strings.Replace(questionParamsJSON, `{"label":"SQLite","description":"One file."}`, `{"label":"SQLite","description":"One file.","preview":"`+preview+`"}`, 1)
	m, _ := openQuestion(t, data)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 12})
	rows := panelRows(panelAsk, m.screen.height)
	if rows != 8 {
		t.Fatalf("window height 12 gives %d card rows, want 8", rows)
	}
	lines := m.questionLines(40, rows)
	if len(lines) > rows {
		t.Fatalf("%d lines in %d rows", len(lines), rows)
	}
	for _, line := range lines {
		if ansi.StringWidth(line) > 40 {
			t.Fatalf("line wider than 40 cells: %q", line)
		}
	}
}

func TestQuestionCardShrinkOrder(t *testing.T) {
	t.Parallel()
	options := make([]string, 0, 8)
	for i := range 8 {
		options = append(options, `{"label":"L`+string(rune('a'+i))+`","description":"d"}`)
	}
	data := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"H","question":"Question text.","isOther":false,"isSecret":false,"multiSelect":false,"options":[` +
		strings.Join(options, ",") + `]}]}`
	m, _ := openQuestion(t, data)
	lines := m.questionLines(60, 9)
	joined := strings.Join(lines, "\n")
	if strings.Contains(joined, "Question text.") {
		t.Fatalf("question text stayed at 9 rows: %q", joined)
	}
	if !strings.Contains(joined, "Question 1 of 1") || !strings.Contains(joined, "└─ ↑/↓ move") {
		t.Fatalf("title or hint dropped: %q", joined)
	}
	if !strings.Contains(joined, "↓ more") {
		t.Fatalf("options did not scroll: %q", joined)
	}
	short, _ := openQuestion(t, lettersQuestion(3, "Question text.", "image: a"))
	lines = short.questionLines(60, 7)
	joined = strings.Join(lines, "\n")
	if !strings.Contains(joined, "Question text.") || strings.Contains(joined, "image: a") {
		t.Fatalf("the preview must drop before the question text: %q", joined)
	}
	if !strings.Contains(joined, "Lc") {
		t.Fatalf("options that fit were dropped: %q", joined)
	}
}

func lettersQuestion(count int, question, preview string) string {
	options := make([]string, 0, count)
	for i := range count {
		options = append(options, `{"label":"L`+string(rune('a'+i))+`","description":"d","preview":"`+preview+`"}`)
	}
	return `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"H","question":"` + question + `","isOther":false,"isSecret":false,"multiSelect":false,"options":[` +
		strings.Join(options, ",") + `]}]}`
}

func TestQuestionCardScrollKeepsCursorRow(t *testing.T) {
	t.Parallel()
	m, _ := openQuestion(t, lettersQuestion(5, "Pick one.", ""))
	m, _ = drive(t, m, press('j', 0, "j"))
	m, _ = drive(t, m, press('j', 0, "j"))
	lines := m.questionLines(40, 4)
	for _, line := range lines {
		if strings.Contains(line, "┃ ›") && strings.Contains(line, "3. Lc") {
			return
		}
	}
	t.Fatalf("the cursor option is hidden in 4 rows: %q", strings.Join(lines, "\n"))
}

func TestQuestionCardDigitBeyondRowsIsIgnored(t *testing.T) {
	t.Parallel()
	two := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Database","question":"Which database?","isOther":false,"isSecret":false,"multiSelect":false,` +
		`"options":[{"label":"SQLite","description":"One file."},{"label":"Postgres","description":"A server."}]}]}`
	m, answer := openQuestion(t, two)
	m, _ = drive(t, m, press('3', 0, "3"))
	if m.typingAnswer() {
		t.Fatal("digit 3 opened the answer line for a row that is not on screen")
	}
	if len(m.approvals) != 1 {
		t.Fatalf("digit 3 closed the card: %d requests", len(m.approvals))
	}
	select {
	case got := <-answer:
		t.Fatalf("digit 3 answered the question: %#v", got)
	default:
	}
	_, _ = drive(t, m, press('s', 0, "s"))
	if got := answerJSON(t, answer); !strings.Contains(got, `"q1":{"answers":[]}`) {
		t.Fatalf("answer = %s", got)
	}
}

func TestQuestionCardCommitShowsStoredText(t *testing.T) {
	t.Parallel()
	data := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Name","question":"Name?","isOther":false,"isSecret":false,"multiSelect":false},` +
		`{"id":"q2","header":"Owner","question":"Owner?","isOther":false,"isSecret":false,"multiSelect":false}]}`
	m, _ := openQuestion(t, data)
	m.approvals[0].card.typed[1] = "stored"
	m, _ = drive(t, m, press('a', 0, "a"))
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if got := m.input.field.value(); got != "stored" {
		t.Fatalf("input field = %q after the commit moved to a question that holds %q", got, "stored")
	}
}

const answerResolved = `{"threadId":"t1","requestId":"req-1","resolution":"answered"}`

func answerWarning(threadID, requestID, field string) notificationMsg {
	return notificationMsg{method: "warning", params: []byte(`{"threadId":"` + threadID + `","message":"invalid text: ` + field +
		`: text_too_large","requestId":"` + requestID + `","reason":"text_too_large","field":"` + field + `"}`)}
}

func submissions(m model) []string {
	var keys []string
	for _, entry := range m.out.entries {
		if strings.HasPrefix(entry.key, "question-submission:") {
			keys = append(keys, entry.key, entry.text)
		}
	}
	return keys
}

func sentAnswer(t *testing.T, answer chan requestAnswer) string {
	t.Helper()
	select {
	case got := <-answer:
		raw, err := json.Marshal(got.value, json.Deterministic(true))
		if err != nil || got.err != nil {
			t.Fatalf("answer = %v, %v", got.err, err)
		}
		return string(raw)
	default:
		t.Fatal("no answer was sent")
		return ""
	}
}

const firstSubmission = `question-submission:"t1":"turn-1":"call-q":"req-1":1`

func sentQuestion(t *testing.T) (model, chan requestAnswer) {
	t.Helper()
	m, answer := openQuestion(t, questionParamsJSON)
	m = typeKeys(t, m, "14")
	m = typeKeys(t, m, " Ready  ")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, press('s', 0, "s"))
	if got := sentAnswer(t, answer); got != `{"answers":{"q1":{"answers":["SQLite"]},"q2":{"answers":[" Ready  "]}}}` {
		t.Fatalf("first answer = %s", got)
	}
	return m, answer
}

func TestQuestionSubmitKeepsTheCardUntilAMatchingWarning(t *testing.T) {
	t.Parallel()
	m, answer := sentQuestion(t)
	card := m.approvals[0].card
	if !card.sent || m.notice != "Answer sent; waiting for the agent" {
		t.Fatalf("sent card = %v, notice %q", card.sent, m.notice)
	}
	first := []string{firstSubmission, "Database: Local submission #1: SQLite\nFeatures: Local submission #1: Ready"}
	if got := submissions(m); !reflect.DeepEqual(got, first) {
		t.Fatalf("entries = %q, want %q", got, first)
	}
	m, _ = drive(t, m, press('s', 0, "s"))
	m, _ = drive(t, m, answerWarning("t2", "req-1", "answers.q2.answers[0]"))
	m, _ = drive(t, m, answerWarning("t1", "req-9", "answers.q2.answers[0]"))
	if len(answer) != 0 || card.attempts != 1 || !card.sent || len(submissions(m)) != 2 {
		t.Fatalf("keys or foreign warnings moved a sent card: %d queued, attempt %d, sent %v", len(answer), card.attempts, card.sent)
	}
}

func TestQuestionWarningReopensTheAnswerForACorrection(t *testing.T) {
	t.Parallel()
	m, answer := sentQuestion(t)
	card := m.approvals[0].card
	m, _ = drive(t, m, answerWarning("t1", "req-1", "answers.q2.answers[0]"))
	if card.sent || card.index != 1 || !card.typing || m.input.field.value() != " Ready  " {
		t.Fatalf("warning reopened sent=%v index=%d typing=%v field=%q", card.sent, card.index, card.typing, m.input.field.value())
	}
	assertContains(t, m.notice, "Answer not accepted: invalid text: answers.q2.answers[0]: text_too_large")
	m = typeKeys(t, m, "!")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, press('s', 0, "s"))
	if got := sentAnswer(t, answer); got != `{"answers":{"q1":{"answers":["SQLite"]},"q2":{"answers":[" Ready  !"]}}}` {
		t.Fatalf("corrected answer = %s", got)
	}
	want := []string{
		firstSubmission, "Database: Local submission #1: SQLite\nFeatures: Local submission #1: Ready",
		`question-submission:"t1":"turn-1":"call-q":"req-1":2`, "Database: Local submission #2: SQLite\nFeatures: Local submission #2: Ready  !",
	}
	if got := submissions(m); !reflect.DeepEqual(got, want) {
		t.Fatalf("entries = %q, want %q", got, want)
	}
	assertResolvedStaysClosed(t, m)
}

func assertResolvedStaysClosed(t *testing.T, m model) {
	t.Helper()
	m, _ = drive(t, m, notificationMsg{method: "serverRequest/resolved", params: []byte(answerResolved)})
	if len(m.approvals) != 0 || m.notice != "Question answered" {
		t.Fatalf("resolution left %d cards and notice %q", len(m.approvals), m.notice)
	}
	m, _ = drive(t, m, answerWarning("t1", "req-1", "answers.q2.answers[0]"))
	if len(m.approvals) != 0 {
		t.Fatal("a warning after resolution reopened the card")
	}
}

func TestQuestionDismissAndSkipCommitLocalSubmissions(t *testing.T) {
	t.Parallel()
	m, answer := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, press('s', 0, "s"))
	if got := sentAnswer(t, answer); got != `{"answers":{"q1":{"answers":[]},"q2":{"answers":[]}}}` {
		t.Fatalf("skip answer = %s", got)
	}
	if got := submissions(m); len(got) != 2 || got[1] != "Database: Local submission #1: skip requested\nFeatures: Local submission #1: skip requested" {
		t.Fatalf("skip entries = %q", got)
	}
	m, _ = drive(t, m, answerWarning("t1", "req-1", "answers.q1.answers[0]"))
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if got := sentAnswer(t, answer); got != `{"answers":{}}` {
		t.Fatalf("dismiss answer = %s", got)
	}
	if got := submissions(m); len(got) != 4 || got[3] != "Database: Local submission #2: dismiss requested\nFeatures: Local submission #2: dismiss requested" {
		t.Fatalf("dismiss entries = %q", got)
	}
	if len(m.approvals) != 0 {
		t.Fatal("a dismissed card stayed open")
	}
}

func TestQuestionEntryKeysKeepEveryFieldApart(t *testing.T) {
	t.Parallel()
	card := func(turnID, itemID string) *questionCard {
		return newQuestionCard(rpc.ToolRequestUserInputParams{ThreadID: "t1", TurnID: turnID, ItemID: itemID}, "req-1", nil)
	}
	if a, b := card(`a":"b`, "c").entryKey("question-submission"), card("a", `b":"c`).entryKey("question-submission"); a == b {
		t.Fatalf("two identities share the key %s", a)
	}
	m, _ := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, press('s', 0, "s"))
	m, _ = drive(t, m, notificationMsg{method: "serverRequest/resolved", params: []byte(answerResolved)})
	second := strings.Replace(questionParamsJSON, `"turnId":"turn-1"`, `"turnId":"turn-2"`, 1)
	m, _ = drive(t, m, questionRequest(make(chan requestAnswer, 1), second))
	m, _ = drive(t, m, press('s', 0, "s"))
	got := submissions(m)
	want := []string{`question-submission:"t1":"turn-1":"call-q":"req-1":1`, `question-submission:"t1":"turn-2":"call-q":"req-1":1`}
	if len(got) != 2*len(want) || got[0] != want[0] || got[2] != want[1] {
		t.Fatalf("two turns printed %q, want keys %q", got, want)
	}
}

func previewQuestion() string {
	options := `{"label":"SQLite","description":"One file.","preview":"l1\nl2\nl3\nl4\nl5\nl6"},{"label":"Postgres","description":"A server."}`
	return `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Database","question":"Which database?","isOther":false,"isSecret":false,"multiSelect":false,"options":[` + options + `]}]}`
}

func TestQuestionPreviewRowPointsToReview(t *testing.T) {
	t.Parallel()
	m, _ := openQuestion(t, previewQuestion())
	card := strings.Join(m.questionLines(60, 20), "\n")
	assertContains(t, stripANSI(card), "l1", "5 more lines · ctrl+o")
	for _, hidden := range []string{"l2", "l3", "l4", "l5", "l6"} {
		if strings.Contains(stripANSI(card), hidden) {
			t.Fatalf("preview line %q is on the card:\n%s", hidden, card)
		}
	}
}

func TestQuestionReviewShowsFullPreview(t *testing.T) {
	t.Parallel()
	m, answer := openQuestion(t, previewQuestion())
	m, _ = drive(t, m, press('j', 0, "j"))
	cursorRow := m.approvals[0].card.cursor[0]
	m, _ = drive(t, m, press('o', tea.ModCtrl, ""))
	if m.review == nil {
		t.Fatal("ctrl+o did not open the review on the open question")
	}
	text := stripANSI(m.review.live.text)
	assertContains(t, text, "Question 1 of 1 · Database", "1. SQLite", "2. Postgres", "l1", "l2", "l3", "l4", "l5", "l6")
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if m.review != nil {
		t.Fatal("esc did not close the review")
	}
	if len(m.approvals) != 1 || m.approvals[0].card == nil {
		t.Fatal("esc closed the question card")
	}
	if got := m.approvals[0].card.cursor[0]; got != cursorRow {
		t.Fatalf("cursor row moved from %d to %d", cursorRow, got)
	}
	select {
	case got := <-answer:
		t.Fatalf("esc in the review sent an answer: %#v", got)
	default:
	}
}

func TestQuestionCardStripsEscapes(t *testing.T) {
	t.Parallel()
	data := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"H\u001b[31m","question":"Q\u001b]0;x\u0007?","isOther":false,"isSecret":false,"multiSelect":false,"options":[{"label":"A","description":"d"},{"label":"B","description":"d"}]}]}`
	m, _ := openQuestion(t, data)
	view := strings.Join(m.questionLines(60, 13), "\n")
	if strings.Contains(view, "\x1b]") || !strings.Contains(view, "Q") {
		t.Fatalf("escapes reached the view: %q", view)
	}
}

func TestWaitingOnUserInputState(t *testing.T) {
	t.Parallel()
	m, _ := openQuestion(t, questionParamsJSON)
	if m.thread.state != stateAwaitingInput {
		t.Fatalf("state = %q", m.thread.state)
	}
	if got := stateWord(m.thread.state, 0); got != "Waiting" {
		t.Fatalf("word = %q", got)
	}
}

func TestQuestionNotificationOSC99(t *testing.T) {
	t.Parallel()
	m := playTerminal(newTestModel(nil, Options{}))
	m.conn.product = "dalgona"
	m.thread.title = "demo"
	m.screen.notifications = true
	m, _ = drive(t, m, tea.BlurMsg{})
	m.screen.env = tea.EnvMsg{"TERM=xterm-kitty"}
	answer := make(chan requestAnswer, 1)
	updated, command := m.Update(questionRequest(answer, questionParamsJSON))
	m = updated.(model)
	_ = m
	got := flatten(command)
	want := []tea.Msg{tea.RawMsg{Msg: "\x1b]99;;dalgona: Waiting for your answer · demo\x07"}}
	if !reflect.DeepEqual(got, want) && !containsMsg(got, want[0]) {
		t.Fatalf("notification = %#v, want %#v", got, want)
	}
}

const sandboxQuestionJSON = `{"threadId":"t1","turnId":"turn-1","itemId":"call-sb","isBlocking":true,"questions":[` +
	`{"id":"sandbox","header":"Sandbox","question":"The harness sandbox is not available. How should commands in this thread run?","isOther":false,"isSecret":false,"multiSelect":false,` +
	`"options":[{"label":"Accept","description":"Run without a sandbox. Every command asks first."},{"label":"Dangerously accept","description":"Run without a sandbox under approval mode auto."}]}]}`

func TestAskDockSteady(t *testing.T) {
	t.Parallel()
	m, _ := openQuestion(t, questionParamsJSON)
	keys := make([]tea.KeyPressMsg, 50)
	for i := range keys {
		keys[i] = tea.KeyPressMsg{Code: tea.KeyDown}
	}
	m = assertSteadyDock(t, m, keys)
	if got := len(m.dockLines()); got != 16 {
		t.Fatalf("dock has %d rows with a question open, want 16", got)
	}
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	typing := make([]tea.KeyPressMsg, 50)
	for i := range typing {
		typing[i] = press('a', 0, "a")
	}
	assertSteadyDock(t, m, typing)
	if got := len(m.dockLines()); got != 16 {
		t.Fatalf("dock has %d rows in answer mode, want 16", got)
	}
}

func TestSandboxChoiceIsAskCard(t *testing.T) {
	t.Parallel()
	m, answer := openQuestion(t, sandboxQuestionJSON)
	view := strings.Join(m.dockLines(), "\n")
	for _, want := range []string{"Question 1 of 1 · Sandbox", "Accept", "Dangerously accept"} {
		if !strings.Contains(view, want) {
			t.Fatalf("sandbox card lacks %q:\n%s", want, view)
		}
	}
	if strings.Contains(view, "Other") {
		t.Fatalf("sandbox card shows an Other row:\n%s", view)
	}
	_, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
	if got := answerJSON(t, answer); got != `{"answers":{}}` {
		t.Fatalf("answer = %s, want the cancel shape", got)
	}
}

func assertQuestionUnsent(t *testing.T, answer chan requestAnswer) {
	t.Helper()
	select {
	case got := <-answer:
		t.Fatalf("question answered before submission: %#v", got)
	default:
	}
}

func TestQuestionReviewNavigationAndEditing(t *testing.T) {
	t.Parallel()
	m, answer := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, press(tea.KeyLeft, 0, ""))
	text := stripANSI(strings.Join(m.questionLines(100, 12), "\n"))
	assertContains(t, text, "Review · 2 questions", "Database", "Features", "(no answer)")
	assertQuestionUnsent(t, answer)
	m, _ = drive(t, m, press(tea.KeyTab, 0, ""))
	m, _ = drive(t, m, press('1', 0, "1"))
	m, _ = drive(t, m, press('1', 0, "1"))
	m, _ = drive(t, m, press('3', 0, "3"))
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	text = stripANSI(strings.Join(m.questionLines(100, 12), "\n"))
	assertContains(t, text, "Review · 2 questions", "SQLite", "Search; Sync")
	assertQuestionUnsent(t, answer)
	m, _ = drive(t, m, press(tea.KeyTab, 0, ""))
	m, _ = drive(t, m, press('2', 0, "2"))
	m, _ = drive(t, m, press(tea.KeyRight, 0, ""))
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	got := answerJSON(t, answer)
	assertContains(t, got, `"q1":{"answers":["Postgres"]}`, `"q2":{"answers":["Search","Sync"]}`)
	if strings.Contains(got, "SQLite") {
		t.Fatalf("editing left the old answer: %s", got)
	}
}

func TestQuestionReviewScrollStaysBounded(t *testing.T) {
	t.Parallel()
	params, err := decodeQuestion(jsontext.Value(questionParamsJSON))
	if err != nil {
		t.Fatal(err)
	}
	for i := range 8 {
		q := params.Questions[0]
		q.ID = string(rune('a' + i))
		q.Header = "Header " + q.ID
		params.Questions = append(params.Questions, q)
	}
	data, err := json.Marshal(params)
	if err != nil {
		t.Fatal(err)
	}
	m, answer := openQuestion(t, string(data))
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 12})
	m, _ = drive(t, m, press(tea.KeyLeft, 0, ""))
	start := stripANSI(strings.Join(m.questionLines(80, 8), "\n"))
	assertContains(t, start, "Database", "↑/↓ scroll")
	for range 100 {
		m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	}
	end := stripANSI(strings.Join(m.questionLines(80, 8), "\n"))
	m, _ = drive(t, m, press(tea.KeyTab, 0, ""))
	m, _ = drive(t, m, press(tea.KeyLeft, 0, ""))
	if got := stripANSI(strings.Join(m.questionLines(80, 8), "\n")); got != start {
		t.Fatalf("reentering Review did not reset scrolling:\n%s", got)
	}
	assertContains(t, end, "Header h", "Enter submit")
	if strings.Contains(end, "│ Database") {
		t.Fatalf("review did not scroll: %s", end)
	}
	for range 100 {
		m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
	}
	if got := stripANSI(strings.Join(m.questionLines(80, 8), "\n")); got != start {
		t.Fatalf("scroll did not return to start:\n%s", got)
	}
	assertQuestionUnsent(t, answer)
}

func TestQuestionReviewKeepsSTextAndDismissesEmptyRequest(t *testing.T) {
	t.Parallel()
	m, answer := openQuestion(t, lettersQuestion(1, "Pick.", ""))
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	_, _ = drive(t, m, press('s', 0, "s"))
	assertContains(t, answerJSON(t, answer), `"answers":["La"]`)
	free := `{"threadId":"t1","turnId":"turn-1","itemId":"free","questions":[{"id":"name","header":"Name","question":"Name?"}]}`
	m, answer = openQuestion(t, free)
	m, _ = drive(t, m, press('s', 0, "s"))
	assertQuestionUnsent(t, answer)
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	assertContains(t, stripANSI(strings.Join(m.questionLines(80, 12), "\n")), "Review · 1 question", "│   s")
	_, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	assertContains(t, answerJSON(t, answer), `"answers":["s"]`)
	empty := make(chan requestAnswer, 1)
	m = newTestModel(nil, Options{})
	m, _ = drive(t, m, questionRequest(empty, `{"questions":[]}`))
	if len(m.approvals) != 0 {
		t.Fatal("empty request opened a card")
	}
	if got := answerJSON(t, empty); got != `{"answers":{}}` {
		t.Fatalf("empty request = %s", got)
	}
}

func TestQuestionAnswerPreservesCanonicalText(t *testing.T) {
	t.Parallel()
	free := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Name","question":"Name?","isOther":false,"isSecret":false,"multiSelect":false,"options":null}]}`
	m, answer := openQuestion(t, free)
	text := "ab\u200ee\u0301\U000F0001"
	for _, r := range text {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, press('s', 0, "s"))
	_ = m
	got := answerJSON(t, answer)
	var decoded struct {
		Answers map[string]struct {
			Answers []string `json:"answers"`
		} `json:"answers"`
	}
	if err := json.Unmarshal([]byte(got), &decoded); err != nil {
		t.Fatalf("answer %s does not decode: %v", got, err)
	}
	if !reflect.DeepEqual(decoded.Answers["q1"].Answers, []string{text}) {
		t.Fatalf("answer = %s, want the canonical text %q", got, text)
	}
}

func TestQuestionOptionLiteralEscapeDoesNotMimicControl(t *testing.T) {
	t.Parallel()
	data := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Choice","question":"Which?","isOther":false,"isSecret":false,"multiSelect":false,` +
		`"options":[{"label":"Pick<U+200E>","description":"d"},{"label":"Pick\u200e","description":"e"}]}]}`
	m, _ := openQuestion(t, data)
	card := m.approvals[0].card
	assertContains(t, card.optionLine(&m.screen, 80, 0), "Pick<U+003C>U+200E>")
	assertContains(t, card.optionLine(&m.screen, 80, 1), "Pick<U+200E>")
	assertContains(t, strings.Join(card.reviewLines(), "\n"), "Pick<U+003C>U+200E>")
}

func TestQuestionOptionLabelSentByteExact(t *testing.T) {
	t.Parallel()
	label := "Pick\u2028\u200e\U000F0001"
	data := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Choice","question":"Which?","isOther":false,"isSecret":false,"multiSelect":false,` +
		`"options":[{"label":"` + label + `","description":"d"},{"label":"Other","description":"e"}]}]}`
	m, answer := openQuestion(t, data)
	m, _ = drive(t, m, press('1', 0, "1"))
	_, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	got := answerJSON(t, answer)
	var decoded struct {
		Answers map[string]struct {
			Answers []string `json:"answers"`
		} `json:"answers"`
	}
	if err := json.Unmarshal([]byte(got), &decoded); err != nil {
		t.Fatalf("answer %s does not decode: %v", got, err)
	}
	if !reflect.DeepEqual(decoded.Answers["q1"].Answers, []string{label}) {
		t.Fatalf("answer = %s, want the exact server label %q", got, label)
	}
}
