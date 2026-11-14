package tui

import (
	"cmp"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

type loginPhase int

const (
	loginProviders loginPhase = iota
	loginMethods
	loginAPIKey
	loginWaiting
)

const (
	loginProvidersHint = "↑/↓ choose · Enter select · Esc close"
	loginMethodsHint   = "↑/↓ choose · Enter select · Esc back"
	loginKeyHint       = "Enter save · Esc back"
	loginWaitingHint   = "Esc cancel the sign-in"
)

type loginMethod struct {
	label string
	kind  string
}

type loginPanel struct {
	phase     loginPhase
	loading   bool
	want      string
	providers []rpc.AccountProvider
	provider  rpc.AccountProvider
	methods   []loginMethod
	cursor    int
	err       string
	loginID   string
	url       string
	code      string
	cancelled bool
}

func (p *loginPanel) move(delta, count int) {
	if count == 0 {
		p.cursor = 0
		return
	}
	p.cursor = (p.cursor + delta + count) % count
}

func (m *model) openLogin(provider string) tea.Cmd {
	m.login = &loginPanel{loading: true, want: sanitizeText(provider)}
	m.input.field = textField{}
	return m.call("providers", "dalgona/account/providers/list", struct{}{}, new(rpc.AccountProvidersResponse), "")
}

func (m *model) closeLogin() {
	m.login = nil
	m.input.field = textField{}
}

func (m *model) closeWaitingLogin() {
	if m.login != nil && m.login.phase == loginWaiting {
		m.closeLogin()
	}
}

func loginMethodsFor(mode string) []loginMethod {
	switch mode {
	case "apiKey":
		return []loginMethod{{label: "API key", kind: "apiKey"}}
	case "browser":
		return []loginMethod{{label: "Browser", kind: "chatgpt"}, {label: "Device code", kind: "chatgptDeviceCode"}}
	}
	return nil
}

func loginModeText(mode string) string {
	switch mode {
	case "apiKey":
		return "API key"
	case "browser":
		return "browser or device code"
	}
	return "terminal"
}

func signedInText(signedIn bool) string {
	if signedIn {
		return "Signed in"
	}
	return "Not signed in"
}

func loginError(err error) string {
	return sanitizeText(strings.Join(strings.Fields(err.Error()), " "))
}

func loginActions(err, hint string) []string {
	if err == "" {
		return []string{hint}
	}
	return []string{err, hint}
}

func (m *model) loginPanelView() panelView {
	switch m.login.phase {
	case loginProviders:
		return m.loginProvidersView()
	case loginMethods:
		return m.loginMethodsView()
	case loginAPIKey:
		return m.loginKeyView()
	case loginWaiting:
		return m.loginWaitingView()
	}
	return panelView{}
}

func (m *model) loginProvidersView() panelView {
	panel := m.login
	view := panelView{kind: "login", layout: panelLogin, title: "Log in to a provider", cursor: panel.cursor}
	if panel.loading {
		view.body = []string{"Loading providers…"}
		view.cursor = -1
	}
	for _, p := range panel.providers {
		view.body = append(view.body, p.ID+"  "+signedInText(p.SignedIn)+"  "+loginModeText(p.Login))
	}
	view.actions = loginActions(panel.err, loginProvidersHint)
	return view
}

func (m *model) loginMethodsView() panelView {
	panel := m.login
	view := panelView{kind: "login", layout: panelLogin, title: "Log in to " + panel.provider.ID, cursor: panel.cursor}
	if len(panel.methods) == 0 {
		view.body = []string{"Run `" + cmp.Or(m.conn.product, "dalgona") + " login " + panel.provider.ID + "` in a shell to sign in."}
		view.cursor = -1
	}
	for _, method := range panel.methods {
		view.body = append(view.body, method.label)
	}
	view.actions = loginActions(panel.err, loginMethodsHint)
	return view
}

func (m *model) loginKeyView() panelView {
	panel := m.login
	return panelView{
		kind:    "login",
		layout:  panelLogin,
		title:   "API key for " + panel.provider.ID,
		body:    []string{"Type the key. It is saved only when " + panel.provider.ID + " accepts it."},
		actions: loginActions(panel.err, loginKeyHint),
		cursor:  -1,
	}
}

func (m *model) loginWaitingView() panelView {
	panel := m.login
	view := panelView{kind: "login", layout: panelLogin, title: "Log in to " + panel.provider.ID, cursor: -1}
	switch {
	case panel.url == "":
		view.body = []string{"Starting sign-in…"}
	case panel.code != "":
		view.body = []string{"Open " + panel.url + " and enter code " + panel.code + "."}
	default:
		view.body = []string{"Open " + panel.url + " to sign in.", "Waiting for the provider to confirm."}
	}
	hint := loginWaitingHint
	if panel.cancelled {
		hint = "Cancelling sign-in…"
	}
	view.actions = loginActions("", hint)
	return view
}

func (m *model) loginKey(msg tea.KeyPressMsg) tea.Cmd {
	switch m.login.phase {
	case loginProviders:
		return m.providersKey(msg.String())
	case loginMethods:
		return m.methodsKey(msg.String())
	case loginAPIKey:
		return m.apiKeyKey(msg)
	case loginWaiting:
		if msg.String() == "esc" {
			return m.cancelLogin()
		}
	}
	return nil
}

func (m *model) providersKey(key string) tea.Cmd {
	panel := m.login
	count := len(panel.providers)
	switch key {
	case "up", "ctrl+p":
		panel.move(-1, count)
	case "down", "ctrl+n":
		panel.move(1, count)
	case "enter":
		if count > 0 {
			m.openMethods()
		}
	case "esc":
		m.closeLogin()
	}
	return nil
}

func (m *model) methodsKey(key string) tea.Cmd {
	panel := m.login
	count := len(panel.methods)
	switch key {
	case "up", "ctrl+p":
		panel.move(-1, count)
	case "down", "ctrl+n":
		panel.move(1, count)
	case "enter":
		if count > 0 {
			return m.startLogin(panel.methods[panel.cursor])
		}
	case "esc":
		m.backToProviders()
	}
	return nil
}

func (m *model) apiKeyKey(msg tea.KeyPressMsg) tea.Cmd {
	switch key := msg.String(); key {
	case "enter":
		return m.submitAPIKey()
	case "esc":
		m.input.field = textField{}
		m.login.err = ""
		m.login.phase = loginMethods
		m.login.cursor = 0
	default:
		if m.input.field.edit(key) || (msg.Text != "" && m.input.field.insert(msg.Text)) {
			m.login.err = ""
		}
	}
	return nil
}

func (m *model) openMethods() {
	panel := m.login
	panel.provider = panel.providers[panel.cursor]
	panel.methods = loginMethodsFor(panel.provider.Login)
	panel.phase = loginMethods
	panel.cursor = 0
	panel.err = ""
}

func (m *model) backToProviders() {
	panel := m.login
	panel.phase = loginProviders
	panel.err = ""
	panel.cursor = max(0, slices.IndexFunc(panel.providers, func(p rpc.AccountProvider) bool {
		return p.ID == panel.provider.ID
	}))
}

func (m *model) startLogin(method loginMethod) tea.Cmd {
	panel := m.login
	panel.err = ""
	if method.kind == "apiKey" {
		panel.phase = loginAPIKey
		m.input.field = textField{}
		return nil
	}
	panel.phase = loginWaiting
	panel.loginID, panel.url, panel.code = "", "", ""
	params := rpc.AccountLoginStartParams{Type: method.kind, Provider: panel.provider.ID}
	return m.call("login", "account/login/start", params, new(rpc.LoginAccountResponse), "")
}

func (m *model) submitAPIKey() tea.Cmd {
	key := strings.TrimSpace(m.input.field.value())
	if key == "" {
		m.login.err = "Enter the API key."
		return nil
	}
	m.login.err = ""
	params := rpc.AccountLoginStartParams{Type: "apiKey", Provider: m.login.provider.ID, APIKey: key}
	return m.call("loginKey", "account/login/start", params, new(rpc.LoginAccountResponse), "")
}

func (m *model) cancelLogin() tea.Cmd {
	panel := m.login
	if panel.loginID == "" {
		panel.cancelled = true
		return nil
	}
	id := panel.loginID
	m.closeLogin()
	return m.call("loginCancel", "account/login/cancel", rpc.AccountLoginCancelParams{LoginID: id}, new(rpc.CancelLoginAccountResponse), "")
}

func (m *model) applyLoginResult(msg resultMsg) (tea.Cmd, bool) {
	switch msg.op {
	case "providers":
		m.applyProviders(msg)
	case "loginKey":
		m.applyKeyResult(msg)
	case "login":
		return m.applyLoginStart(msg), true
	case "loginCancel":
		if msg.err != nil {
			m.errorf("cancel sign-in failed: %v", msg.err)
		}
	default:
		return nil, false
	}
	return nil, true
}

func (p *loginPanel) setProviders(response *rpc.AccountProvidersResponse) bool {
	p.providers = response.Providers
	p.cursor = 0
	if p.want == "" {
		return false
	}
	index := slices.IndexFunc(p.providers, func(prov rpc.AccountProvider) bool { return prov.ID == p.want })
	if index < 0 {
		p.err = "unknown provider " + p.want + "; choose one below"
		return false
	}
	p.cursor = index
	return true
}

func (m *model) applyProviders(msg resultMsg) {
	panel := m.login
	if panel == nil {
		return
	}
	panel.loading = false
	if msg.err != nil {
		panel.err = loginError(msg.err)
		return
	}
	response, ok := msg.value.(*rpc.AccountProvidersResponse)
	if !ok {
		panel.err = "providers response has an unexpected shape; reconnect and retry"
		return
	}
	if panel.setProviders(response) {
		m.openMethods()
	}
}

func (m *model) applyKeyResult(msg resultMsg) {
	if m.login == nil {
		return
	}
	if msg.err != nil {
		m.login.err = loginError(msg.err)
		return
	}
	m.closeLogin()
}

func (m *model) applyLoginStart(msg resultMsg) tea.Cmd {
	if m.login != nil && m.login.cancelled {
		return m.finishCancelledStart(msg)
	}
	if msg.err != nil {
		if m.login == nil {
			return m.applyResultError(msg)
		}
		m.loginRetry(loginError(msg.err))
		return nil
	}
	if m.login != nil {
		m.loginStarted(msg.value)
	}
	return m.applyLogin(msg.value)
}

func (m *model) finishCancelledStart(msg resultMsg) tea.Cmd {
	m.closeLogin()
	if msg.err != nil {
		m.errorf("%s", loginError(msg.err))
		return nil
	}
	response, ok := msg.value.(*rpc.LoginAccountResponse)
	if !ok || response.LoginID == "" {
		return nil
	}
	return m.call("loginCancel", "account/login/cancel", rpc.AccountLoginCancelParams{LoginID: response.LoginID}, new(rpc.CancelLoginAccountResponse), "")
}

func (m *model) loginStarted(value any) {
	if m.login.phase != loginWaiting {
		return
	}
	response, ok := value.(*rpc.LoginAccountResponse)
	if !ok {
		m.loginRetry("sign-in response has an unexpected shape")
		return
	}
	url := sanitizeText(cmp.Or(response.AuthURL, response.VerificationURL))
	if url == "" {
		m.loginRetry("the provider returned no sign-in link")
		return
	}
	m.login.loginID = response.LoginID
	m.login.url = url
	m.login.code = sanitizeText(response.UserCode)
}

func (m *model) loginRetry(text string) {
	if m.login == nil || m.login.phase != loginWaiting {
		return
	}
	m.login.phase = loginMethods
	m.login.err = sanitizeText(text)
	m.login.loginID, m.login.url, m.login.code = "", "", ""
	m.login.cursor = 0
}

func (m *model) loginTyping() bool {
	return m.login != nil && m.login.phase == loginAPIKey
}
