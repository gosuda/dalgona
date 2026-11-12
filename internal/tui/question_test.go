package tui

import (
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"reflect"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
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
	m := newTestModel(nil, Options{}, nil)
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
	_, _ = drive(t, m, press('2', 0, "2"))
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

func TestQuestionCardFreeText(t *testing.T) {
	t.Parallel()
	free := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Name","question":"Name?","isOther":false,"isSecret":false,"multiSelect":false,"options":null}]}`
	m, _ := openQuestion(t, free)
	if !m.approvals[0].card.typing {
		t.Fatal("free-text question did not start in input mode")
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

func TestQuestionCardCommitsAnswerRow(t *testing.T) {
	t.Parallel()
	m, _ := openQuestion(t, questionParamsJSON)
	m, _ = drive(t, m, press('s', 0, "s"))
	if len(m.out.pending) == 0 && len(m.out.entries) == 0 {
		t.Fatal("answer row was not committed")
	}
	before := len(m.out.entries)
	m.commit("question:t1:call-q", []string{"again"})
	if len(m.out.entries) != before {
		t.Fatal("second commit with the same key was kept")
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
	m := playTerminal(newTestModel(nil, Options{}, nil))
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
