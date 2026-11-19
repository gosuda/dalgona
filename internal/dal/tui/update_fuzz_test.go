package tui

import (
	"errors"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	fuzzMaxOps    = 64
	fuzzMaxBulk   = 1
	fuzzAnswerCap = 4
)

type fuzzReader struct {
	data []byte
	at   int
}

func (r *fuzzReader) more() bool {
	return r.at < len(r.data)
}

func (r *fuzzReader) next() byte {
	if r.at >= len(r.data) {
		return 0
	}
	b := r.data[r.at]
	r.at++
	return b
}

func (r *fuzzReader) pick(n int) int {
	return int(r.next()) % n
}

type fuzzSession struct {
	t       *testing.T
	m       model
	width   int
	height  int
	now     time.Time
	answers []chan requestAnswer
	rows    int
	bulk    int
}

type fuzzOp func(s *fuzzSession, r *fuzzReader, settle bool)

const (
	opKey = iota
	opRune
	opPhrase
	opPaste
	opResize
	opSize
	opWheel
	opClick
	opFocus
	opNotify
	opRequest
	opResult
	opDisconnect
	opTick
	opRows
	opAdvance
)

var fuzzOps = [...]fuzzOp{
	opKey:        fuzzKey,
	opRune:       fuzzRune,
	opPhrase:     fuzzPhrase,
	opPaste:      fuzzPaste,
	opResize:     fuzzResize,
	opSize:       fuzzSize,
	opWheel:      fuzzWheel,
	opClick:      fuzzClick,
	opFocus:      fuzzFocus,
	opNotify:     fuzzNotify,
	opRequest:    fuzzRequest,
	opResult:     fuzzResult,
	opDisconnect: fuzzDisconnect,
	opTick:       fuzzTick,
	opRows:       fuzzRows,
	opAdvance:    fuzzAdvance,
}

var fuzzKeys = []tea.KeyPressMsg{
	press(tea.KeyEnter, 0, ""),
	press(tea.KeyEscape, 0, ""),
	press(tea.KeyTab, 0, ""),
	press(tea.KeyTab, tea.ModShift, ""),
	press(tea.KeyBackspace, 0, ""),
	press(tea.KeyDelete, 0, ""),
	press(tea.KeyUp, 0, ""),
	press(tea.KeyDown, 0, ""),
	press(tea.KeyLeft, 0, ""),
	press(tea.KeyRight, 0, ""),
	press(tea.KeyHome, 0, ""),
	press(tea.KeyEnd, 0, ""),
	press(tea.KeyPgUp, 0, ""),
	press(tea.KeyPgDown, 0, ""),
	press(' ', 0, " "),
	press('c', tea.ModCtrl, ""),
	press('o', tea.ModCtrl, ""),
	press('z', tea.ModCtrl, ""),
	press('r', tea.ModCtrl, ""),
	press('j', tea.ModCtrl, ""),
	press('v', tea.ModCtrl, ""),
	press('w', tea.ModCtrl, ""),
	press('u', tea.ModCtrl, ""),
	press('k', tea.ModCtrl, ""),
	press('a', tea.ModCtrl, ""),
	press('e', tea.ModCtrl, ""),
	press(tea.KeyEnter, tea.ModAlt, ""),
	press(tea.KeyUp, tea.ModAlt, ""),
	press(tea.KeyLeft, tea.ModAlt, ""),
	press(tea.KeyRight, tea.ModAlt, ""),
	press('q', tea.ModAlt, ""),
}

var fuzzRunes = []rune(" !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~한글あ中😀éÅ\u0301\u200d\u3000")

var fuzzPhrases = []string{
	"/",
	"/help",
	"/model",
	"/resume",
	"/new",
	"/compact",
	"/login",
	"/tree",
	"/revert",
	"/fork",
	"/advisor",
	"/review",
	"/settings",
	"//escaped",
	"/nope",
	"@",
	"@internal/",
	"!ls",
	"hello world",
	"한글 입력 테스트 안녕하세요",
	strings.Repeat("long ", 60),
}

var fuzzSizes = []tea.WindowSizeMsg{
	{Width: 0, Height: 0},
	{Width: 1, Height: 1},
	{Width: 2, Height: 2},
	{Width: 20, Height: 6},
	{Width: 3, Height: 3},
	{Width: 1, Height: 24},
	{Width: 0, Height: 24},
	{Width: 80, Height: 0},
	{Width: 80, Height: 1},
	{Width: 40, Height: 8},
	{Width: 80, Height: 24},
	{Width: 200, Height: 60},
}

var fuzzBulk = sync.OnceValue(func() string {
	return strings.Repeat("0123456789abcdef\n", pasteLimit/17+1)[:pasteLimit]
})

var fuzzOverLimit = sync.OnceValue(func() string {
	return fuzzBulk() + "x"
})

func fuzzLines(n int) string {
	var text strings.Builder
	for i := range n {
		text.WriteString("line ")
		text.WriteString(strconv.Itoa(i))
		text.WriteByte('\n')
	}
	return text.String()
}

func fuzzPastes() []string {
	return []string{
		"pasted",
		fuzzLines(30),
		"a\tb\tc\n\tindented\tline\n\t\t\n",
		"\x1b[31mred\x1b[0m \x1b]0;title\x07 \x1b[2J\x1b[H\x1b[?1049h \x1b[201~ \x1bP1$r\x1b\\",
		"ok \xff\xfe\xc0\xaf\xed\xa0\x80 tail \xe4\xb8",
		"한글 붙여넣기\n두 번째 줄 😀 e\u0301\n",
		"\r\n\r\nwindows\r\nlines\r\n",
		fuzzBulk(),
		fuzzOverLimit(),
	}
}

func fuzzNote(method, params string) notificationMsg {
	return notificationMsg{method: method, params: []byte(params)}
}

var fuzzDeltaEscaper = strings.NewReplacer(`\`, `\\`, `"`, `\"`, "\n", `\n`, "\t", `\t`, "\r", `\r`, "\x1b", `\u001b`)

func fuzzDelta(method, itemID, text string) notificationMsg {
	return fuzzNote(method, `{"threadId":"t1","turnId":"turn-1","itemId":"`+itemID+`","delta":"`+fuzzDeltaEscaper.Replace(text)+`"}`)
}

const (
	fuzzAgentStarted   = `{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":""}}`
	fuzzAgentDone      = `{"threadId":"t1","turnId":"turn-1","completedAtMs":2,"item":{"type":"agentMessage","id":"a1","text":""}}`
	fuzzCommandStarted = `{"threadId":"t1","turnId":"turn-1","startedAtMs":3,"item":{"type":"commandExecution","id":"c1","command":"seq 30","commandActions":[],"cwd":"/work","status":"inProgress","aggregatedOutput":null}}`
	fuzzFileStarted    = `{"threadId":"t1","turnId":"turn-1","startedAtMs":5,"item":{"type":"fileChange","id":"f1","status":"inProgress","changes":[]}}`
	fuzzFileDone       = `{"threadId":"t1","turnId":"turn-1","completedAtMs":6,"item":{"type":"fileChange","id":"f1","status":"completed","changes":[{"path":"a.go","kind":{"type":"update"},"diff":"--- a/a.go\n+++ b/a.go\n@@ -1,2 +1,2 @@\n-old\t한글\n+new\t한글\n context\n","additions":1,"deletions":1}]}}`
	fuzzPatchUpdated   = `{"threadId":"t1","turnId":"turn-1","itemId":"f1","changes":[{"path":"internal/very/long/path/to/a/file/that/keeps/going/a.go","kind":{"type":"update"},"diff":"--- a/a.go\n+++ b/a.go\n@@ -1 +1 @@\n-old\n+new\n","additions":1,"deletions":1}]}`
	fuzzPlanUpdated    = `{"threadId":"t1","turnId":"turn-1","plan":[{"step":"read the code","status":"completed"},{"step":"write the fix 한글 plan step that is long enough to wrap around a narrow window","status":"inProgress"},{"step":"run the tests","status":"pending"}]}`
	fuzzTurnFailed     = `{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"failed","error":{"message":"provider unavailable"}}}`
	fuzzTurnInterrupt  = `{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"interrupted"}}`
)

func fuzzNotifications() []notificationMsg {
	return []notificationMsg{
		fuzzNote("thread/started", invThreadStarted),
		fuzzNote("turn/started", invTurnStarted),
		fuzzNote("item/started", fuzzAgentStarted),
		fuzzDelta("item/agentMessage/delta", "a1", "# Title\n\nSome **bold** and `code` text with a [link](https://example.com).\n\n- one\n- two\n\n> quote\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n"),
		fuzzDelta("item/agentMessage/delta", "a1", "```go\nfunc f() {\n\treturn\n}\n```\n"),
		fuzzDelta("item/agentMessage/delta", "a1", "```py\nprint(1)\n"),
		fuzzDelta("item/agentMessage/delta", "a1", "a\tb\tc\n\tindented\ttext\n"),
		fuzzDelta("item/agentMessage/delta", "a1", "한글 텍스트 안녕하세요 세계 👩\u200d👩\u200d👧\u200d👦 e\u0301 あいうえお中文字\n"),
		fuzzDelta("item/agentMessage/delta", "a1", strings.Repeat("x", 300)),
		fuzzDelta("item/agentMessage/delta", "a1", fuzzLines(50)),
		fuzzDelta("item/reasoning/textDelta", "r1", "thinking about the plan\nstep two\n"),
		fuzzNote("item/completed", fuzzAgentDone),
		fuzzNote("item/started", fuzzCommandStarted),
		fuzzDelta("item/commandExecution/outputDelta", "c1", fuzzLines(30)),
		fuzzDelta("item/commandExecution/outputDelta", "c1", "\x1b[31mred\x1b[0m\r\nline\ttab\n한글 출력\n\x1b[2J\x1b[H"),
		fuzzNote("item/completed", invCommandDone),
		fuzzNote("item/started", fuzzFileStarted),
		fuzzNote("item/fileChange/patchUpdated", fuzzPatchUpdated),
		fuzzNote("item/completed", fuzzFileDone),
		fuzzNote("turn/plan/updated", fuzzPlanUpdated),
		fuzzNote("error", `{"threadId":"t1","turnId":"turn-1","willRetry":true,"error":{"message":"stream reset"}}`),
		fuzzNote("error", `{"threadId":"t1","turnId":"turn-1","willRetry":false,"error":{"message":"provider unavailable 한글 with a very long explanation that needs more than one row to read"}}`),
		fuzzNote("dalgona/retry/wait", `{"threadId":"t1","turnId":"turn-1","remainingMs":125000}`),
		fuzzNote("turn/completed", `{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`),
		fuzzNote("turn/completed", fuzzTurnInterrupt),
		fuzzNote("turn/completed", fuzzTurnFailed),
		fuzzNote("item/agentMessage/delta", `{"threadId":"t1"`),
	}
}

func fuzzRequests() []serverRequestMsg {
	command := strings.Repeat("go test ./internal/tui/... -run TestFrame -count=1\\n", 40)
	return []serverRequestMsg{
		{method: "item/commandExecution/requestApproval", requestID: "r1", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"cmd1","startedAtMs":1,"command":"go test ./..."}`)},
		{method: "item/commandExecution/requestApproval", requestID: "r2", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"cmd2","startedAtMs":1,"command":"` + command + `","cwd":"/work","reason":"한글 reason\twith a tab"}`)},
		{method: "item/fileChange/requestApproval", requestID: "r3", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"f1","startedAtMs":1,"reason":"edit a.go"}`)},
		{method: "item/fileChange/requestApproval", requestID: "r4", params: []byte(`{"threadId":"child","turnId":"turn-2","itemId":"fc1","startedAtMs":1,"reason":"edit a.go"}`)},
		{method: "item/tool/requestUserInput", requestID: "r5", params: []byte(questionParamsJSON)},
		{method: "item/tool/requestUserInput", requestID: "r6", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"q","isBlocking":true,"questions":[{"id":"q1","header":"Secret","question":"Which token?","isOther":true,"isSecret":true,"multiSelect":false,"options":[]}]}`)},
		{method: "item/tool/requestUserInput", requestID: "r7", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"q","isBlocking":false,"questions":[]}`)},
		{method: "item/unknown/request", requestID: "r8", params: []byte(`{}`)},
		{method: "item/commandExecution/requestApproval", requestID: "r9", params: []byte(`{`)},
	}
}

func fuzzResults() []resultMsg {
	models := make([]rpc.Model, 300)
	for i := range models {
		models[i] = rpc.Model{ID: "model-" + strconv.Itoa(i)}
	}
	return []resultMsg{
		{op: "models", value: &rpc.ModelListResponse{Data: models}},
		{op: "models", value: &rpc.ModelListResponse{Data: []rpc.Model{{ID: "한글-model"}}}},
		{op: "threads", value: &rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "t1", Name: "demo", Preview: "한글 preview\twith a tab"}, {ID: "t2"}}}},
		{op: "threads", value: &rpc.ThreadListResponse{}},
		{op: "initialize", err: errFuzz},
	}
}

var errFuzz = errors.New("dial failed\nretry later")

func fuzzRowSets() [][]string {
	return [][]string{
		{"short row"},
		{strings.Repeat("wide row ", 40)},
		{"한글 row 안녕하세요 " + strings.Repeat("글", 80)},
		{"tab\trow\t\tend", "\x1b[31mcolored\x1b[0m"},
		strings.Split(strings.TrimSuffix(fuzzLines(30), "\n"), "\n"),
		{""},
	}
}

func (s *fuzzSession) send(msg tea.Msg, settle bool) {
	s.t.Helper()
	updated, _ := s.m.Update(msg)
	next, ok := updated.(model)
	if !ok {
		s.t.Fatalf("Update returned %T for %T, want model", updated, msg)
	}
	if settle {
		next = settleFrames(next)
	}
	s.m = next
	s.drainAnswers()
	s.assertView(msg)
	s.resumeAfterSuspend()
}

func (s *fuzzSession) resumeAfterSuspend() {
	s.t.Helper()
	if s.m.leaving && !s.m.suspending && !s.m.quitting {
		s.send(tea.ResumeMsg{}, true)
	}
}

func (s *fuzzSession) drainAnswers() {
	for _, answer := range s.answers {
		for len(answer) > 0 {
			<-answer
		}
	}
}

func (s *fuzzSession) assertView(msg tea.Msg) {
	s.t.Helper()
	content := s.m.View().Content
	defer func() {
		if s.t.Failed() {
			s.t.Logf("after %T %+v at %dx%d", msg, msg, s.width, s.height)
		}
	}()
	if s.height < 2 {
		s.assertNoDock(content)
		return
	}
	assertFrame(s.t, strings.Split(content, "\n"), max(s.width, 2), s.height)
}

func (s *fuzzSession) assertNoDock(content string) {
	s.t.Helper()
	if s.m.review == nil && strings.TrimSpace(ansi.Strip(content)) != "" {
		s.t.Fatalf("height %d shows a dock row, want none: %q", s.height, content)
	}
}

func (s *fuzzSession) done() bool {
	return s.m.quitting && s.m.finishQuit() != nil
}

func fuzzKey(s *fuzzSession, r *fuzzReader, settle bool) {
	s.send(fuzzKeys[r.pick(len(fuzzKeys))], settle)
}

func fuzzRune(s *fuzzSession, r *fuzzReader, settle bool) {
	char := fuzzRunes[r.pick(len(fuzzRunes))]
	s.send(press(char, 0, string(char)), settle)
}

func fuzzPhrase(s *fuzzSession, r *fuzzReader, settle bool) {
	phrase := []rune(fuzzPhrases[r.pick(len(fuzzPhrases))])
	for i, char := range phrase {
		s.send(press(char, 0, string(char)), settle && i == len(phrase)-1)
	}
}

func fuzzPaste(s *fuzzSession, r *fuzzReader, settle bool) {
	pastes := fuzzPastes()
	text := pastes[r.pick(len(pastes))]
	if len(text) >= pasteLimit {
		if s.bulk >= fuzzMaxBulk {
			text = pastes[0]
		}
		s.bulk++
	}
	s.send(tea.PasteMsg{Content: text}, settle)
}

func (s *fuzzSession) resize(width, height int, settle bool) {
	s.t.Helper()
	s.width, s.height = width, height
	s.send(tea.WindowSizeMsg{Width: width, Height: height}, settle)
}

func fuzzResize(s *fuzzSession, r *fuzzReader, settle bool) {
	width := int(r.next()) % 201
	height := int(r.next()) % 61
	s.resize(width, height, settle)
}

func fuzzSize(s *fuzzSession, r *fuzzReader, settle bool) {
	size := fuzzSizes[r.pick(len(fuzzSizes))]
	s.resize(size.Width, size.Height, settle)
}

var fuzzWheels = []tea.MouseButton{tea.MouseWheelUp, tea.MouseWheelDown, tea.MouseWheelLeft, tea.MouseWheelRight}

var fuzzButtons = []tea.MouseButton{tea.MouseLeft, tea.MouseRight, tea.MouseMiddle}

func fuzzWheel(s *fuzzSession, r *fuzzReader, settle bool) {
	button := fuzzWheels[r.pick(len(fuzzWheels))]
	s.send(tea.MouseWheelMsg{Button: button, X: int(r.next()) % 201, Y: int(r.next()) % 61}, settle)
}

func fuzzClick(s *fuzzSession, r *fuzzReader, settle bool) {
	button := fuzzButtons[r.pick(len(fuzzButtons))]
	s.send(tea.MouseClickMsg{Button: button, X: int(r.next()) % 201, Y: int(r.next()) % 61}, settle)
}

func fuzzFocus(s *fuzzSession, r *fuzzReader, settle bool) {
	if r.next()&1 == 0 {
		s.send(tea.FocusMsg{}, settle)
		return
	}
	s.send(tea.BlurMsg{}, settle)
}

func fuzzNotify(s *fuzzSession, r *fuzzReader, settle bool) {
	notes := fuzzNotifications()
	s.send(notes[r.pick(len(notes))], settle)
}

func fuzzRequest(s *fuzzSession, r *fuzzReader, settle bool) {
	requests := fuzzRequests()
	request := requests[r.pick(len(requests))]
	request.answer = make(chan requestAnswer, fuzzAnswerCap)
	s.answers = append(s.answers, request.answer)
	s.send(request, settle)
}

func fuzzResult(s *fuzzSession, r *fuzzReader, settle bool) {
	results := fuzzResults()
	s.send(results[r.pick(len(results))], settle)
}

func fuzzDisconnect(s *fuzzSession, _ *fuzzReader, settle bool) {
	s.send(disconnectedMsg{err: errFuzz}, settle)
}

func fuzzTick(s *fuzzSession, _ *fuzzReader, settle bool) {
	s.now = s.now.Add(time.Second)
	s.send(s.m.screen.spin.Tick(), settle)
}

func fuzzRows(s *fuzzSession, r *fuzzReader, settle bool) {
	sets := fuzzRowSets()
	s.rows++
	s.m.commit("fuzz:"+strconv.Itoa(s.rows), slices.Clone(sets[r.pick(len(sets))]))
	s.send(flushMsg{}, settle)
}

func fuzzAdvance(s *fuzzSession, _ *fuzzReader, _ bool) {
	switch {
	case s.m.out.settling:
		s.send(settledMsg{n: s.m.out.settleN}, false)
	case s.m.out.printing:
		s.send(printedMsg{}, false)
	default:
		s.send(flushMsg{}, false)
	}
}

func newFuzzSession(t *testing.T) *fuzzSession {
	s := &fuzzSession{t: t, now: time.Date(2026, 11, 12, 10, 0, 0, 0, time.UTC), width: 80, height: 24}
	s.m = newTestModel(&fakeCaller{}, Options{})
	s.m.conn.clock = func() time.Time { return s.now }
	s.send(tea.WindowSizeMsg{Width: s.width, Height: s.height}, true)
	return s
}

func runFuzzSequence(t *testing.T, data []byte) {
	s := newFuzzSession(t)
	r := &fuzzReader{data: data}
	for range fuzzMaxOps {
		if !r.more() || s.done() {
			return
		}
		code := r.next()
		fuzzOps[int(code>>1)%len(fuzzOps)](s, r, code&1 == 1)
	}
}

func fuzzSeed(steps ...[]byte) []byte {
	return slices.Concat(steps...)
}

func fuzzStep(kind byte, settle bool, args ...byte) []byte {
	code := kind << 1
	if settle {
		code |= 1
	}
	return append([]byte{code}, args...)
}

func FuzzUpdateSequence(f *testing.F) {
	f.Add(fuzzSeed(
		fuzzStep(opNotify, true, 0),
		fuzzStep(opNotify, true, 1),
		fuzzStep(opNotify, true, 2),
		fuzzStep(opNotify, true, 3),
		fuzzStep(opNotify, true, 11),
		fuzzStep(opSize, true, 9),
		fuzzStep(opNotify, true, 24),
	))
	f.Add(fuzzSeed(
		fuzzStep(opPhrase, false, 1),
		fuzzStep(opKey, true, 0),
		fuzzStep(opKey, true, 1),
		fuzzStep(opPhrase, false, 12),
		fuzzStep(opKey, true, 0),
		fuzzStep(opKey, true, 1),
	))
	f.Add(fuzzSeed(
		fuzzStep(opRequest, true, 0),
		fuzzStep(opKey, true, 16),
		fuzzStep(opKey, true, 1),
		fuzzStep(opRequest, true, 4),
		fuzzStep(opKey, true, 7),
		fuzzStep(opRune, true, 'y'-' '),
	))
	f.Add(fuzzSeed(
		fuzzStep(opPaste, true, 1),
		fuzzStep(opSize, true, 3),
		fuzzStep(opSize, true, 1),
		fuzzStep(opSize, true, 10),
		fuzzStep(opPaste, true, 3),
		fuzzStep(opPaste, true, 4),
	))
	f.Add(fuzzSeed(
		fuzzStep(opResult, true, 0),
		fuzzStep(opWheel, true, 1, 5, 5),
		fuzzStep(opClick, true, 0, 2, 3),
		fuzzStep(opKey, true, 1),
		fuzzStep(opKey, true, 17),
		fuzzStep(opKey, true, 17),
	))
	f.Add(fuzzSeed(
		fuzzStep(opNotify, false, 1),
		fuzzStep(opRows, false, 4),
		fuzzStep(opResize, false, 40, 8),
		fuzzStep(opAdvance, false),
		fuzzStep(opTick, false),
		fuzzStep(opDisconnect, true),
		fuzzStep(opFocus, true, 1),
	))
	f.Add(fuzzSeed(
		fuzzStep(opPaste, true, 2),
		fuzzStep(opResize, true, 14, 12),
		fuzzStep(opKey, true, 0),
		fuzzStep(opResize, true, 10, 12),
		fuzzStep(opKey, true, 8),
		fuzzStep(opKey, true, 9),
	))
	f.Fuzz(func(t *testing.T, data []byte) {
		runFuzzSequence(t, data)
	})
}
