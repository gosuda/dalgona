package app

import (
	"bufio"
	"bytes"
	"cmp"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/rpc"
)

type CLIEnv struct {
	Product        config.Product
	Home           string
	Dial           func(context.Context) (io.ReadWriteCloser, error)
	Remote         string
	Stdin          io.Reader
	Stdout, Stderr io.Writer
	OnStatus       func(Status)
	OnChildSpawned func(childID, cwd string)
	TUI            TUIBackend
}

// Status reports presentation progress without naming a presentation
// package, so core orchestration stays independent of terminal widgets.
type Status struct {
	State    string
	Seq      uint64
	ThreadID string
}

// TUIOptions carries everything the interactive presentation layer needs.
type TUIOptions struct {
	OnStatus       func(Status)
	OnChildSpawned func(childID, cwd string)
	Stdin          io.Reader
	Stdout         io.Writer
	ThreadID       string
	Model          string
	Approval       string
	Sandbox        string
	Cwd            string
}

// TUIBackend launches the interactive presentation layer. Command entry
// points wire it, so this package never imports a presentation package.
type TUIBackend func(ctx context.Context, conn io.ReadWriteCloser, opts TUIOptions) error

type cliFlags struct {
	prompt   string
	headless bool
	json     bool
	resume   string
	latest   bool
	model    string
	approval string
	sandbox  string
	dir      string
}

var errUsage = errors.New("usage")

const loginCancelTimeout = 5 * time.Second

func RunCLI(ctx context.Context, env CLIEnv, args []string) int {
	if len(args) > 0 {
		switch args[0] {
		case "login":
			return env.exit(env.login(ctx, args[1:]))
		case "logout":
			return env.exit(env.logout(ctx, args[1:]))
		case "models":
			return env.exit(env.models(ctx))
		}
	}
	f, err := env.parse(args)
	if err != nil {
		return env.exit(err)
	}
	if f.dir != "" {
		if err := os.Chdir(f.dir); err != nil {
			return env.exit(fmt.Errorf("-C %s: %w", f.dir, err))
		}
	}
	if f.headless {
		return env.headless(ctx, f)
	}
	return env.exit(env.interactive(ctx, f))
}

func (env CLIEnv) exit(err error) int {
	switch {
	case err == nil, errors.Is(err, flag.ErrHelp):
		return 0
	case errors.Is(err, errUsage):
		return 2
	}
	fmt.Fprintf(env.Stderr, "%s: %v\n", env.Product, err)
	return 1
}

func (env CLIEnv) parse(args []string) (cliFlags, error) {
	var f cliFlags
	fs := flag.NewFlagSet(string(env.Product), flag.ContinueOnError)
	fs.SetOutput(env.Stderr)
	fs.Func("p", "run one turn with `prompt` and exit", func(v string) error {
		f.prompt, f.headless = v, true
		return nil
	})
	fs.BoolVar(&f.json, "json", false, "with -p, print one JSON status line")
	fs.StringVar(&f.resume, "resume", "", "resume thread `id`")
	fs.BoolVar(&f.latest, "c", false, "continue the latest thread")
	fs.StringVar(&f.model, "m", "", "model `id` for a new thread")
	fs.StringVar(&f.approval, "approval", "", "approval mode: auto, ask, or yolo")
	fs.StringVar(&f.sandbox, "sandbox", "", "sandbox mode: workspace-write or off")
	fs.StringVar(&f.dir, "C", "", "run in `dir`")
	if err := fs.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return f, err
		}
		return f, errUsage
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(env.Stderr, "%s: unexpected argument %q\n", env.Product, fs.Arg(0))
		return f, errUsage
	}
	return f, f.validate(env)
}

func (f cliFlags) validate(env CLIEnv) error {
	if problem := f.problem(); problem != "" {
		fmt.Fprintf(env.Stderr, "%s: %s\n", env.Product, problem)
		return errUsage
	}
	return nil
}

func (f cliFlags) problem() string {
	checks := []struct {
		bad     func() bool
		message string
	}{
		{func() bool { return !slices.Contains([]string{"", "auto", "ask", "yolo"}, f.approval) }, "--approval must be auto, ask, or yolo"},
		{func() bool { return !slices.Contains([]string{"", "workspace-write", "off"}, f.sandbox) }, "--sandbox must be workspace-write or off"},
		{f.emptyPrompt, "-p needs a prompt unless --resume or -c selects a thread"},
		{func() bool { return f.resume != "" && f.latest }, "--resume and -c exclude each other"},
	}
	for _, check := range checks {
		if check.bad() {
			return check.message
		}
	}
	return ""
}

func (f cliFlags) emptyPrompt() bool {
	return f.headless && f.prompt == "" && f.resume == "" && !f.latest
}

func approvalPolicy(mode string) string {
	switch mode {
	case "yolo":
		return "never"
	case "ask":
		return "untrusted"
	case "auto":
		return "on-request"
	}
	return ""
}

func sandboxName(mode string) string {
	if mode == "off" {
		return "danger-full-access"
	}
	return mode
}

type loginRequest struct {
	provider string
	apiKey   string
}

func (env CLIEnv) login(ctx context.Context, args []string) error {
	request, err := env.parseLogin(args)
	if err != nil {
		return err
	}
	if err := request.check(); err != nil {
		return err
	}
	if env.Remote != "" {
		return env.remoteLogin(ctx, request)
	}
	return env.localLogin(ctx, request)
}

func (env CLIEnv) parseLogin(args []string) (loginRequest, error) {
	var request loginRequest
	if len(args) > 0 && !strings.HasPrefix(args[0], "-") {
		request.provider, args = args[0], args[1:]
	}
	fs := flag.NewFlagSet(string(env.Product)+" login", flag.ContinueOnError)
	fs.SetOutput(env.Stderr)
	fs.StringVar(&request.apiKey, "api-key", "", "store `key` for an API-key provider without a prompt")
	if err := fs.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return request, err
		}
		return request, errUsage
	}
	if fs.NArg() > 0 || (request.apiKey != "" && request.provider == "") {
		fmt.Fprintf(env.Stderr, "usage: %s login [provider] [--api-key key]\n", env.Product)
		return request, errUsage
	}
	return request, nil
}

func (r loginRequest) check() error {
	if r.apiKey != "" && !slices.Contains(apiKeyLoginTargets(), r.provider) {
		return fmt.Errorf("login %s: --api-key works only for %s", r.provider, strings.Join(apiKeyLoginTargets(), ", "))
	}
	return nil
}

func (env CLIEnv) localLogin(ctx context.Context, request loginRequest) error {
	store, err := auth.Load(env.Home)
	if err != nil {
		return err
	}
	if request.provider == "" {
		signedIn := map[string]bool{}
		for _, entry := range store.Entries() {
			signedIn[entry.ID] = true
		}
		env.listProviders(signedIn)
		return nil
	}
	if request.apiKey != "" {
		err = store.SetAPIKey(request.provider, request.apiKey)
	} else {
		err = Login(ctx, store, request.provider, env.Stdin, env.Stdout)
	}
	if err != nil {
		return err
	}
	fmt.Fprintf(env.Stdout, "logged in to %s\n", request.provider)
	return nil
}

func (env CLIEnv) listProviders(signedIn map[string]bool) {
	for _, id := range Providers() {
		mark := ""
		if signedIn[id] {
			mark = " (logged in)"
		}
		fmt.Fprintf(env.Stdout, "%s%s\n", id, mark)
	}
}

func (env CLIEnv) remoteLogin(ctx context.Context, request loginRequest) error {
	params, err := env.remoteLoginParams(request)
	if err != nil {
		return err
	}
	session, err := env.connect(ctx)
	if err != nil {
		return err
	}
	defer session.close(env)
	if request.provider == "" {
		return env.showRemoteAccount(ctx, session)
	}
	var started rpc.LoginAccountResponse
	if err := session.client.Call(ctx, "account/login/start", params, &started); err != nil {
		return fmt.Errorf("login %s: %w", request.provider, err)
	}
	if started.LoginID != "" {
		env.printLoginPrompt(request.provider, started)
		if err := session.awaitLogin(ctx, started.LoginID); err != nil {
			return fmt.Errorf("login %s: %w", request.provider, err)
		}
	}
	fmt.Fprintf(env.Stdout, "logged in to %s in remote %s\n", request.provider, env.Remote)
	return nil
}

func (env CLIEnv) remoteLoginParams(request loginRequest) (rpc.AccountLoginStartParams, error) {
	switch {
	case request.provider == "":
		return rpc.AccountLoginStartParams{}, nil
	case slices.Contains(apiKeyLoginTargets(), request.provider):
		key, err := env.readAPIKey(request)
		return rpc.AccountLoginStartParams{Type: "apiKey", Provider: request.provider, APIKey: key}, err
	case request.provider == "chatgpt":
		return rpc.AccountLoginStartParams{Type: "chatgptDeviceCode"}, nil
	case slices.Contains(oauthProviders, request.provider):
		return rpc.AccountLoginStartParams{}, fmt.Errorf(
			"login %s cannot run inside remote %s; run `%s login %s` here, then `%s cloud up %s --copy-auth`",
			request.provider, env.Remote, env.Product, request.provider, env.Product, env.Remote)
	}
	return rpc.AccountLoginStartParams{}, fmt.Errorf("unknown provider %q; known providers: %s", request.provider, strings.Join(Providers(), ", "))
}

func (env CLIEnv) readAPIKey(request loginRequest) (string, error) {
	if request.apiKey != "" {
		return request.apiKey, nil
	}
	fmt.Fprintf(env.Stdout, "Paste the %s API key: ", request.provider)
	key, err := bufio.NewReader(env.Stdin).ReadString('\n')
	if key = strings.TrimSpace(key); key == "" {
		return "", errors.Join(fmt.Errorf("login %s: no API key was entered", request.provider), ignoreEOF(err))
	}
	return key, nil
}

func (env CLIEnv) printLoginPrompt(provider string, started rpc.LoginAccountResponse) {
	if started.UserCode != "" {
		fmt.Fprintf(env.Stdout, "Open %s and enter the code %s to log in to %s.\n", started.VerificationURL, started.UserCode, provider)
		return
	}
	fmt.Fprintf(env.Stdout, "Open this URL to log in to %s:\n%s\n", provider, started.AuthURL)
}

func (env CLIEnv) showRemoteAccount(ctx context.Context, session *cliSession) error {
	var read rpc.AccountReadResponse
	if err := session.client.Call(ctx, "account/read", rpc.AccountReadParams{}, &read); err != nil {
		return err
	}
	state := "not logged in"
	signedIn := map[string]bool{}
	switch {
	case read.Account == nil:
	case read.Account.Type == "chatgpt":
		signedIn["chatgpt"] = true
		state = "logged in to chatgpt"
		if read.Account.Email != "" {
			state += " as " + read.Account.Email
		}
	default:
		state = "logged in with an API key"
	}
	env.listProviders(signedIn)
	fmt.Fprintf(env.Stdout, "remote %s: %s\n", env.Remote, state)
	return nil
}

func (env CLIEnv) logout(ctx context.Context, args []string) error {
	if len(args) != 1 {
		fmt.Fprintf(env.Stderr, "usage: %s logout <provider>\n", env.Product)
		return errUsage
	}
	if env.Remote != "" {
		return env.remoteLogout(ctx, args[0])
	}
	store, err := auth.Load(env.Home)
	if err != nil {
		return err
	}
	return store.Logout(args[0])
}

func (env CLIEnv) remoteLogout(ctx context.Context, provider string) error {
	session, err := env.connect(ctx)
	if err != nil {
		return err
	}
	defer session.close(env)
	if err := session.client.Call(ctx, "account/logout", rpc.AccountLogoutParams{Provider: provider}, nil); err != nil {
		return fmt.Errorf("logout %s: %w", provider, err)
	}
	return nil
}

func (env CLIEnv) models(ctx context.Context) error {
	session, err := env.connect(ctx)
	if err != nil {
		return err
	}
	defer session.close(env)
	var list rpc.ModelListResponse
	if err := session.client.Call(ctx, "model/list", rpc.ModelListParams{}, &list); err != nil {
		return err
	}
	for _, m := range list.Data {
		mark := ""
		if m.IsDefault {
			mark = " (default)"
		}
		fmt.Fprintf(env.Stdout, "%s%s\n", m.ID, mark)
	}
	return nil
}

func (env CLIEnv) interactive(ctx context.Context, f cliFlags) error {
	threadID := f.resume
	if f.latest {
		id, err := env.latestThread(ctx)
		if err != nil {
			return err
		}
		threadID = id
	}
	conn, err := env.Dial(ctx)
	if err != nil {
		return err
	}
	if env.TUI == nil {
		return errors.Join(errors.New("app: interactive terminal is not wired"), closeQuiet(conn))
	}
	cwd, err := os.Getwd()
	if err != nil {
		return errors.Join(err, closeQuiet(conn))
	}
	runErr := env.TUI(ctx, conn, TUIOptions{
		OnStatus: env.OnStatus, OnChildSpawned: env.OnChildSpawned, Stdin: env.Stdin, Stdout: env.Stdout,
		ThreadID: threadID, Model: f.model, Approval: f.approval, Sandbox: f.sandbox, Cwd: cwd,
	})
	return errors.Join(runErr, closeQuiet(conn))
}

func closeQuiet(conn io.Closer) error {
	if err := conn.Close(); err != nil && !errors.Is(err, io.ErrClosedPipe) {
		return err
	}
	return nil
}

func (env CLIEnv) latestThread(ctx context.Context) (string, error) {
	session, err := env.connect(ctx)
	if err != nil {
		return "", err
	}
	defer session.close(env)
	return session.latest(ctx)
}

type cliSession struct {
	conn   io.ReadWriteCloser
	client *rpc.Client
	book   *turnBook
}

func (env CLIEnv) connect(ctx context.Context) (*cliSession, error) {
	dialed, err := env.Dial(ctx)
	if err != nil {
		return nil, err
	}
	conn := &watchedConn{ReadWriteCloser: dialed, lost: make(chan struct{})}
	book := newTurnBook(env.Stderr)
	book.lost = conn.lost
	client, err := rpc.Dial(conn, book)
	if err != nil {
		return nil, errors.Join(err, closeQuiet(conn))
	}
	s := &cliSession{conn: conn, client: client, book: book}
	params := rpc.InitializeParams{ClientInfo: rpc.ClientInfo{Name: string(env.Product), Version: "cli"}}
	if err := client.Initialize(ctx, params, &rpc.InitializeResult{}); err != nil {
		s.close(env)
		return nil, err
	}
	return s, nil
}

var errConnectionLost = errors.New("the daemon connection closed")

type watchedConn struct {
	io.ReadWriteCloser
	lost chan struct{}
	once sync.Once
}

func (w *watchedConn) Read(p []byte) (int, error) {
	n, err := w.ReadWriteCloser.Read(p)
	if err != nil {
		w.once.Do(func() { close(w.lost) })
	}
	return n, err
}

func (s *cliSession) close(env CLIEnv) {
	if err := errors.Join(closeQuiet(s.client), closeQuiet(s.conn)); err != nil {
		fmt.Fprintf(env.Stderr, "%s: close: %v\n", env.Product, err)
	}
}

func (s *cliSession) latest(ctx context.Context) (string, error) {
	var list rpc.ThreadListResponse
	if err := s.client.Call(ctx, "thread/list", rpc.ThreadListParams{Limit: 1}, &list); err != nil {
		return "", err
	}
	if len(list.Data) == 0 {
		return "", errors.New("no thread to continue")
	}
	return list.Data[0].ID, nil
}

func (s *cliSession) awaitLogin(ctx context.Context, loginID string) error {
	note, err := s.book.waitLogin(ctx, loginID)
	if errors.Is(err, errConnectionLost) {
		return err
	}
	if err != nil {
		return errors.Join(err, s.cancelLogin(ctx, loginID))
	}
	if !note.Success {
		return errors.New(cmp.Or(note.Error, "the login did not complete"))
	}
	return nil
}

func (s *cliSession) cancelLogin(ctx context.Context, loginID string) error {
	cancelCtx, stop := context.WithTimeout(context.WithoutCancel(ctx), loginCancelTimeout)
	defer stop()
	params := rpc.AccountLoginCancelParams{LoginID: loginID}
	if err := s.client.Call(cancelCtx, "account/login/cancel", params, &rpc.CancelLoginAccountResponse{}); err != nil {
		return fmt.Errorf("cancel login: %w", err)
	}
	return nil
}

type openedThread struct {
	id     string
	active string
	last   *turnResult
}

func (s *cliSession) open(ctx context.Context, f cliFlags) (openedThread, error) {
	id := f.resume
	if f.latest {
		latest, err := s.latest(ctx)
		if err != nil {
			return openedThread{}, err
		}
		id = latest
	}
	if id == "" {
		cwd, err := os.Getwd()
		if err != nil {
			return openedThread{}, err
		}
		var started rpc.ThreadStartResponse
		params := rpc.ThreadStartParams{Model: f.model, ApprovalPolicy: approvalPolicy(f.approval), Sandbox: sandboxName(f.sandbox), Cwd: cwd}
		err = s.client.Call(ctx, "thread/start", params, &started)
		return openedThread{id: started.Thread.ID}, err
	}
	var resumed rpc.ThreadResumeResponse
	if err := s.client.Call(ctx, "thread/resume", rpc.ThreadResumeParams{ThreadID: id}, &resumed); err != nil {
		return openedThread{}, err
	}
	opened := openedThread{id: id}
	if turns := resumed.Thread.Turns; len(turns) > 0 {
		last := turns[len(turns)-1]
		if last.Status == "inProgress" {
			opened.active = last.ID
		}
		result := resultOf(last)
		opened.last = &result
	}
	return opened, nil
}

func resultOf(turn rpc.Turn) turnResult {
	result := turnResult{status: turn.Status, stop: "stop"}
	switch {
	case turn.Error != nil:
		result.stop = turn.Error.Message
	case turn.Status != "completed":
		result.stop = turn.Status
	}
	for _, it := range turn.Items {
		if it.Type == "agentMessage" && it.Text != "" {
			result.texts = append(result.texts, it.Text)
		}
	}
	return result
}

func (env CLIEnv) headless(ctx context.Context, f cliFlags) int {
	session, err := env.connect(ctx)
	if err != nil {
		return env.exit(err)
	}
	defer session.close(env)
	opened, err := session.open(ctx, f)
	if err != nil {
		return env.exit(err)
	}
	result, err := session.settle(ctx, opened, f.prompt)
	if err != nil {
		return env.exit(err)
	}
	return env.report(opened.id, result, f.json)
}

func (s *cliSession) settle(ctx context.Context, opened openedThread, prompt string) (turnResult, error) {
	if opened.active != "" {
		result, err := s.book.wait(ctx, opened.active)
		if err != nil || prompt == "" {
			return result, err
		}
	}
	if prompt != "" {
		return s.turn(ctx, opened.id, prompt)
	}
	if opened.last == nil {
		return turnResult{}, fmt.Errorf("thread %s has no turn", opened.id)
	}
	return *opened.last, nil
}

func (s *cliSession) turn(ctx context.Context, threadID, prompt string) (turnResult, error) {
	var started rpc.TurnStartResponse
	params := rpc.TurnStartParams{ThreadID: threadID, Input: []rpc.InputItem{{Type: "text", Text: prompt}}}
	if err := s.client.Call(ctx, "turn/start", params, &started); err != nil {
		return turnResult{}, err
	}
	return s.book.wait(ctx, started.Turn.ID)
}

func (env CLIEnv) report(threadID string, result turnResult, asJSON bool) int {
	if asJSON {
		line, err := statusLine(threadID, result)
		if err != nil {
			return env.exit(err)
		}
		fmt.Fprintf(env.Stdout, "%s\n", line)
	} else if text := strings.Join(result.texts, "\n\n"); text != "" {
		fmt.Fprintf(env.Stdout, "%s\n", text)
	}
	if result.status != "completed" {
		if !asJSON {
			fmt.Fprintf(env.Stderr, "%s: turn %s: %s\n", env.Product, result.status, result.stop)
		}
		return 1
	}
	return 0
}

func statusLine(threadID string, result turnResult) ([]byte, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	tokens := []jsontext.Token{
		jsontext.BeginObject,
		jsontext.String("thread"), jsontext.String(threadID),
		jsontext.String("status"), jsontext.String(result.status),
		jsontext.String("stop_reason"), jsontext.String(result.stop),
		jsontext.EndObject,
	}
	for _, token := range tokens {
		if err := enc.WriteToken(token); err != nil {
			return nil, fmt.Errorf("encode status line: %w", err)
		}
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte{'\n'}), nil
}

type turnResult struct {
	status string
	stop   string
	texts  []string
}

type turnBook struct {
	stderr     io.Writer
	mu         sync.Mutex
	changed    chan struct{}
	parkNotify chan struct{}
	done       map[string]turnResult
	texts      map[string][]string
	logins     map[string]rpc.AccountLoginCompletedNotification
	lost       <-chan struct{}
}

func newTurnBook(stderr io.Writer) *turnBook {
	return &turnBook{
		stderr: stderr, changed: make(chan struct{}), done: map[string]turnResult{}, texts: map[string][]string{},
		logins: map[string]rpc.AccountLoginCompletedNotification{},
	}
}

func (b *turnBook) Notification(method string, params jsontext.Value) {
	switch method {
	case "item/completed":
		var note rpc.ItemCompletedNotification
		if json.Unmarshal(params, &note) == nil && note.Item.Type == "agentMessage" && note.Item.Text != "" {
			b.mu.Lock()
			b.texts[note.TurnID] = append(b.texts[note.TurnID], note.Item.Text)
			b.mu.Unlock()
		}
	case "turn/completed":
		var note rpc.TurnCompletedNotification
		if json.Unmarshal(params, &note) == nil {
			b.finish(note.Turn)
		}
	case "account/login/completed":
		var note rpc.AccountLoginCompletedNotification
		if json.Unmarshal(params, &note) == nil {
			b.mu.Lock()
			b.logins[note.LoginID] = note
			b.signal()
			b.mu.Unlock()
		}
	}
}

func (b *turnBook) finish(turn rpc.Turn) {
	result := resultOf(turn)
	b.mu.Lock()
	defer b.mu.Unlock()
	result.texts = b.texts[turn.ID]
	b.done[turn.ID] = result
	b.signal()
}

func (b *turnBook) signal() {
	close(b.changed)
	b.changed = make(chan struct{})
}

func (b *turnBook) Request(_ context.Context, method string, params jsontext.Value) (any, error) {
	if !strings.HasSuffix(method, "/requestApproval") {
		return nil, fmt.Errorf("unsupported server request %s", method)
	}
	var ask rpc.CommandExecutionRequestApprovalParams
	if err := json.Unmarshal(params, &ask); err != nil {
		return nil, err
	}
	fmt.Fprintf(b.stderr, "declined %s: no interactive client\n", cmp.Or(ask.Command, ask.Reason, ask.ItemID))
	return rpc.CommandExecutionRequestApprovalResponse{Decision: "decline"}, nil
}

func (b *turnBook) wait(ctx context.Context, turnID string) (turnResult, error) {
	var result turnResult
	err := b.await(ctx, func() (ok bool) {
		result, ok = b.done[turnID]
		return ok
	})
	return result, err
}

func (b *turnBook) waitLogin(ctx context.Context, loginID string) (rpc.AccountLoginCompletedNotification, error) {
	var note rpc.AccountLoginCompletedNotification
	err := b.await(ctx, func() (ok bool) {
		note, ok = b.logins[loginID]
		return ok
	})
	return note, err
}

func (b *turnBook) await(ctx context.Context, ready func() bool) error {
	for {
		ok, changed := b.check(ready)
		if ok {
			return nil
		}
		b.notifyParked()
		select {
		case <-changed:
		case <-b.lost:
			if ok, _ := b.check(ready); ok {
				return nil
			}
			return errConnectionLost
		case <-ctx.Done():
			return ctx.Err()
		}
	}
}

func (b *turnBook) notifyParked() {
	if b.parkNotify == nil {
		return
	}
	select {
	case b.parkNotify <- struct{}{}:
	default:
	}
}

func (b *turnBook) check(ready func() bool) (bool, <-chan struct{}) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return ready(), b.changed
}
