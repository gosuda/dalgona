package main

import (
	"cmp"
	"context"
	"crypto/rand"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"log/slog"
	"maps"
	"net/url"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/mcp"
	"github.com/gosuda/dalgona/internal/dalgona/report"
)

const (
	authFlowAPIKey          = "apiKey"
	authFlowBrowserCallback = "browserCallback"
	authFlowManualCode      = "manualCode"
	authFlowDeviceCode      = "deviceCode"
	authFlowCursorDeepLink  = "cursorDeepLink"
)

const (
	authAwaitingInput = "awaitingInput"
	authWaiting       = "waiting"
	authExchanging    = "exchanging"
	authSucceeded     = "succeeded"
	authFailed        = "failed"
	authCancelled     = "cancelled"
	authExpired       = "expired"
)

const (
	authKindProvider    = "provider"
	authKindMCP         = "mcp"
	authKindReporter    = "reporter"
	reporterTargetID    = "failure-reporting"
	reporterMethodApp   = "app"
	boxTokenEnvironment = "DALGONA_BOX_TOKEN" //nolint:gosec // environment variable name, not a credential value
	authConnectionLimit = 4
	authSetupMargin     = time.Minute
	apiKeyLimit         = 4096
	pastedLimit         = 16384
	authTimeLayout      = "2006-01-02T15:04:05.000Z07:00"
)

var (
	authFlowOrder     = []string{authFlowAPIKey, authFlowBrowserCallback, authFlowManualCode, authFlowDeviceCode, authFlowCursorDeepLink}
	authCallbackModes = map[string]string{authFlowBrowserCallback: "backendLoopback", authFlowManualCode: "manualPaste", authFlowDeviceCode: "device"}
	authWindows       = map[string]time.Duration{
		authFlowAPIKey: 2 * time.Minute, authFlowBrowserCallback: 2 * time.Minute, authFlowManualCode: 2 * time.Minute,
		authFlowDeviceCode: 15 * time.Minute, authFlowCursorDeepLink: 25 * time.Minute,
	}
	authDisplayNames = map[string]string{
		"anthropic": "Anthropic", "antigravity": "Antigravity", "chatgpt": "ChatGPT", "cloudflare": "Cloudflare",
		"copilot": "GitHub Copilot", "cursor": "Cursor", "devin": "Devin", "google": "Google Gemini",
		"openai": "OpenAI", "openrouter": "OpenRouter", "sandbox0": "sandbox0", "sprites": "Sprites",
	}
)

type (
	authTarget struct {
		Kind     string `json:"kind"`
		ID       string `json:"id,omitzero"`
		Name     string `json:"name,omitzero"`
		ThreadID string `json:"threadId,omitzero"`
	}
	authDescriptor struct {
		Target            authTarget `json:"target"`
		DisplayName       string     `json:"displayName"`
		SignedIn          bool       `json:"signedIn"`
		AccountLabel      string     `json:"accountLabel,omitzero"`
		Flows             []string   `json:"flows"`
		UnavailableReason string     `json:"unavailableReason,omitzero"`
	}
	authChallenge struct {
		Kind             string `json:"kind"`
		AuthorizationURL string `json:"authorizationUrl,omitzero"`
		VerificationURL  string `json:"verificationUrl,omitzero"`
		UserCode         string `json:"userCode,omitzero"`
		Prompt           string `json:"prompt,omitzero"`
		ExpiresAt        string `json:"expiresAt"`
	}
	authStatus struct {
		TransactionID string         `json:"transactionId"`
		Target        authTarget     `json:"target"`
		State         string         `json:"state"`
		Challenge     *authChallenge `json:"challenge,omitzero"`
		Error         string         `json:"error,omitzero"`
		ExpiresAt     string         `json:"expiresAt"`
	}
	authResponse struct {
		Kind  string `json:"kind"`
		Value string `json:"value"`
	}
	authStartParams struct {
		Target       *authTarget    `json:"target"`
		Flow         string         `json:"flow"`
		CallbackMode jsontext.Value `json:"callbackMode"`
	}
	authSubmitParams struct {
		TransactionID string        `json:"transactionId"`
		Response      *authResponse `json:"response"`
	}
	authTransactionParams struct {
		TransactionID string `json:"transactionId"`
	}
	authTargetParams struct {
		Target *authTarget `json:"target"`
	}
	authProvidersParams struct {
		ThreadID string `json:"threadId"`
	}
)

type (
	authWork func(ctx context.Context, tx *authTransaction) error
	oauthRun func(ctx context.Context, tx *authTransaction) (oauth.Token, error)
)

type authEntry struct {
	descriptor authDescriptor
	key        string
	workers    map[string]authWork
	saveKey    func(ctx context.Context, key string) error
	forget     func() error
}

type authTransaction struct {
	id, connection, key, flow, label string
	target                           authTarget
	created, expires                 time.Time
	saveKey                          func(ctx context.Context, key string) error
	now                              func() time.Time
	pasted                           chan string
	ready                            chan struct{}
	work                             context.Context
	stop                             context.CancelFunc
	release                          func() bool
	mu                               sync.Mutex
	state, failure                   string
	challenge                        *authChallenge
	challengeEnd                     time.Time
	advertised                       *url.URL
	signalled                        bool
}

type authService struct {
	lookup         func(string) (oauth.Provider, bool)
	config         func() (config.Config, error)
	now            func() time.Time
	log            *slog.Logger
	loopback       bool
	homeOverridden bool
	mu             sync.Mutex
	transactions   map[string]*authTransaction
}

type authCall struct {
	service *authService
	host    *agent.Host
}

func newAuthService(loopback, homeOverridden bool, logger *slog.Logger) *authService {
	return &authService{
		lookup: oauth.ProviderByID, now: time.Now, log: logger, loopback: loopback, homeOverridden: homeOverridden,
		config:       func() (config.Config, error) { return config.Load(config.ProductDalgona, "", config.Flags{}) },
		transactions: map[string]*authTransaction{},
	}
}

func (a *authService) extensions(host *agent.Host) map[string]rpcserver.Extension {
	call := authCall{service: a, host: host}
	return map[string]rpcserver.Extension{
		"dalgona/auth/providers": rpcserver.WithDeadline(rpcserver.MetadataDeadline, call.providers),
		"dalgona/auth/status":    call.status,
		"dalgona/auth/cancel":    call.cancel,
	}
}

func (a *authService) operations(host *agent.Host) map[string]rpcserver.Extension {
	call := authCall{service: a, host: host}
	return map[string]rpcserver.Extension{
		"dalgona/auth/start":  call.start,
		"dalgona/auth/submit": call.submit,
		"dalgona/auth/logout": call.logout,
	}
}

func (a *authService) operationResults() map[string]rpcserver.Extension {
	return map[string]rpcserver.Extension{
		"dalgona/auth/start":  a.liveOperationStatus,
		"dalgona/auth/submit": a.liveOperationStatus,
	}
}

func (a *authService) liveOperationStatus(ctx context.Context, result jsontext.Value) (jsontext.Value, error) {
	status, err := decodeNative[authStatus](result)
	if err != nil {
		return nil, err
	}
	status.Challenge = nil
	if liveState(status.State) {
		if tx, findErr := a.find(ctx, status.TransactionID); findErr == nil {
			status.Challenge = tx.snapshot().Challenge
		}
	}
	return encodeNative(status)
}

func (c authCall) providers(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeNative[authProvidersParams](params)
	if err != nil {
		return nil, err
	}
	targets := []authDescriptor{}
	for _, id := range providerTargetIDs() {
		entry, entryErr := c.providerEntry(id)
		if entryErr != nil {
			return nil, entryErr
		}
		targets = append(targets, entry.descriptor)
	}
	cfg, err := c.service.config()
	if err != nil {
		return nil, err
	}
	for _, name := range slices.Sorted(maps.Keys(cfg.MCPServers)) {
		if cfg.MCPServers[name].OAuth {
			target := authTarget{Kind: authKindMCP, Name: name, ThreadID: p.ThreadID}
			targets = append(targets, c.mcpEntry(cfg, target, cfg.MCPServers[name]).descriptor)
		}
	}
	targets = append(targets, c.service.reporter(cfg).descriptor)
	return encodeNative(struct {
		Targets []authDescriptor `json:"targets"`
	}{targets})
}

func (c authCall) start(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeNative[authStartParams](params)
	if err != nil {
		return nil, err
	}
	if modeErr := checkCallbackMode(p.Flow, p.CallbackMode); modeErr != nil {
		return nil, modeErr
	}
	if p.Target == nil {
		return nil, invalidNative("target is required")
	}
	entry, err := c.resolve(*p.Target)
	if err != nil {
		return nil, err
	}
	if flowErr := entry.offers(p.Flow); flowErr != nil {
		return nil, flowErr
	}
	tx, err := c.service.begin(ctx, entry, p.Flow)
	if err != nil {
		return nil, err
	}
	status := tx.await(ctx)
	status.Challenge = nil
	return encodeNative(status)
}

func (c authCall) submit(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeNative[authSubmitParams](params)
	if err != nil {
		return nil, err
	}
	if p.Response == nil {
		return nil, invalidNative("response is required")
	}
	tx, err := c.service.find(ctx, p.TransactionID)
	if err != nil {
		return nil, err
	}
	var status authStatus
	switch tx.flow {
	case authFlowAPIKey:
		status, err = tx.submitKey(*p.Response)
	case authFlowManualCode:
		status, err = tx.submitPaste(*p.Response)
	default:
		err = invalidNative("a " + tx.flow + " login accepts no response; the provider completes it")
	}
	if err != nil {
		return nil, err
	}
	status.Challenge = nil
	return encodeNative(status)
}

func (c authCall) status(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeNative[authTransactionParams](params)
	if err != nil {
		return nil, err
	}
	tx, err := c.service.find(ctx, p.TransactionID)
	if err != nil {
		return nil, err
	}
	return encodeNative(tx.snapshot())
}

func (c authCall) cancel(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeNative[authTransactionParams](params)
	if err != nil {
		return nil, err
	}
	outcome := "notFound"
	if tx, findErr := c.service.find(ctx, p.TransactionID); findErr == nil {
		outcome = tx.cancelRequest()
	}
	return encodeNative(struct {
		Status string `json:"status"`
	}{outcome})
}

func (c authCall) logout(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeNative[authTargetParams](params)
	if err != nil {
		return nil, err
	}
	if p.Target == nil {
		return nil, invalidNative("target is required")
	}
	entry, err := c.resolve(*p.Target)
	if err != nil {
		return nil, err
	}
	if entry.forget == nil {
		return nil, nativeFailure(rpc.ErrorUnavailable, entry.descriptor.UnavailableReason)
	}
	if err := entry.forget(); err != nil {
		return nil, err
	}
	return encodeNative(struct {
		SignedIn bool `json:"signedIn"`
	}{false})
}

func (c authCall) resolve(target authTarget) (authEntry, error) {
	switch target.Kind {
	case authKindProvider:
		return c.providerEntry(target.ID)
	case authKindMCP:
		cfg, err := c.service.config()
		if err != nil {
			return authEntry{}, err
		}
		spec, err := mcpOAuthSpec(cfg, target.Name)
		if err != nil {
			return authEntry{}, invalidNative(err.Error())
		}
		return c.mcpEntry(cfg, target, spec), nil
	case authKindReporter:
		if target.ID != reporterTargetID {
			return authEntry{}, invalidNative("the reporter target id must be " + reporterTargetID)
		}
		cfg, err := c.service.config()
		if err != nil {
			return authEntry{}, err
		}
		return c.service.reporter(cfg), nil
	}
	return authEntry{}, invalidNative("target kind must be provider, mcp, or reporter")
}

func (c authCall) providerEntry(id string) (authEntry, error) {
	if !slices.Contains(providerTargetIDs(), id) {
		return authEntry{}, invalidNative("unknown provider " + id + "; known providers: " + strings.Join(providerTargetIDs(), ", "))
	}
	entry := newAuthEntry(authTarget{Kind: authKindProvider, ID: id}, "provider/"+id, cmp.Or(authDisplayNames[id], id))
	store := c.host.Auth()
	if store == nil {
		entry.descriptor.UnavailableReason = "the credential store is not available"
		return entry, nil
	}
	entry.descriptor.SignedIn, entry.descriptor.AccountLabel = providerAccount(store, id)
	entry.forget = func() error { return store.Logout(id) }
	if slices.Contains(auth.APIKeyProviders(), id) || slices.Contains(auth.RunnerServices(), id) {
		entry.saveKey = c.saveProviderKey(store, id)
	}
	if definition, ok := c.oauthDefinition(id); ok {
		for flow, run := range c.service.oauthRuns(definition) {
			entry.workers[flow] = c.service.commitProvider(store, id, run)
		}
	}
	entry.descriptor.Flows = entry.flows()
	return entry, nil
}

func (c authCall) oauthDefinition(id string) (oauth.Provider, bool) {
	if !slices.Contains(auth.OAuthProviders(), id) {
		return oauth.Provider{}, false
	}
	return c.service.lookup(id)
}

func (c authCall) saveProviderKey(store *auth.Store, id string) func(context.Context, string) error {
	return func(ctx context.Context, key string) error {
		if slices.Contains(auth.APIKeyProviders(), id) && c.host.VerifyAPIKey(ctx, id, key) != nil {
			return errors.New(id + " did not accept the API key; it was not saved")
		}
		if err := store.SetAPIKey(id, key); err != nil {
			return err
		}
		c.service.announce(ctx, id)
		return nil
	}
}

func (c authCall) mcpEntry(cfg config.Config, target authTarget, spec config.MCPServerSpec) authEntry {
	target = authTarget{Kind: authKindMCP, Name: target.Name, ThreadID: target.ThreadID}
	entry := newAuthEntry(target, "mcp/"+target.Name, target.Name)
	store := c.host.Auth()
	if store == nil {
		entry.descriptor.UnavailableReason = "the credential store is not available"
		return entry
	}
	options := mcp.AuthOptions{Name: target.Name, URL: spec.URL, OAuth: true, OAuthClientID: spec.OAuthClientID, Home: cfg.Home, Store: store}
	entry.descriptor.SignedIn = mcp.AuthStatus(options) == mcp.AuthOAuth
	entry.forget = func() error { return store.Logout(mcp.CredentialKey(target.Name)) }
	work := func(ctx context.Context, tx *authTransaction) error {
		registration, token, err := mcpAuthorize(ctx, target.Name, spec, tx.prompt())
		if err != nil {
			return err
		}
		return mcpCommit(cfg.Home, store, target.Name, registration, token)
	}
	entry.workers[authFlowManualCode] = work
	if c.service.loopback {
		entry.workers[authFlowBrowserCallback] = work
	}
	entry.descriptor.Flows = entry.flows()
	return entry
}

func (a *authService) reporter(cfg config.Config) authEntry {
	entry := newAuthEntry(authTarget{Kind: authKindReporter, ID: reporterTargetID}, "reporter", "Failure reporting")
	inspection := report.Inspect(cfg.Home, cfg.FailureReports.Auth, a.now())
	entry.descriptor.SignedIn = inspection.Authorization == report.AuthorizationReady
	entry.forget = func() error { return report.ForgetAppToken(cfg.Home) }
	entry.descriptor.UnavailableReason = a.reporterBlock(cfg, inspection)
	if entry.descriptor.UnavailableReason == "" {
		entry.workers[authFlowDeviceCode] = func(ctx context.Context, tx *authTransaction) error {
			return report.AcquireAppToken(ctx, cfg.Home, tx.show)
		}
	}
	entry.descriptor.Flows = entry.flows()
	return entry
}

func (a *authService) reporterBlock(cfg config.Config, inspection report.Inspection) string {
	switch {
	case a.homeOverridden:
		return "failure reporting is off for an overridden home"
	case cfg.FailureReports.Auth != reporterMethodApp:
		return "gh mode uses the active github.com account from gh; no login runs"
	case inspection.Authorization == report.AuthorizationUnavailable:
		return "failure reporting authorization is unavailable in this build"
	case !report.ConsentValid(cfg.Home, reporterMethodApp):
		return "accept the failure reporting consent with dalgona failures reporting on before you authorize"
	}
	return ""
}

func (a *authService) oauthRuns(definition oauth.Provider) map[string]oauthRun {
	runs := map[string]oauthRun{}
	if definition.RedirectHost != "" {
		pkce := func(ctx context.Context, tx *authTransaction) (oauth.Token, error) {
			prompt := tx.prompt()
			return oauth.PKCE(ctx, definition, prompt.open, prompt.paste)
		}
		runs[authFlowManualCode] = pkce
		if a.loopback {
			runs[authFlowBrowserCallback] = pkce
		}
	}
	if definition.DeviceURL != "" {
		runs[authFlowDeviceCode] = func(ctx context.Context, tx *authTransaction) (oauth.Token, error) {
			return oauth.DeviceCode(ctx, definition, tx.show)
		}
	}
	if definition.PollURL != "" {
		runs[authFlowCursorDeepLink] = func(ctx context.Context, tx *authTransaction) (oauth.Token, error) {
			return oauth.CursorPoll(ctx, definition, tx.open, nil)
		}
	}
	return runs
}

func (a *authService) commitProvider(store *auth.Store, id string, run oauthRun) authWork {
	return func(ctx context.Context, tx *authTransaction) error {
		token, err := run(ctx, tx)
		if err != nil {
			return err
		}
		credential := auth.Credential{Type: "oauth", Access: token.Access, Refresh: token.Refresh, Account: token.Account}
		if !token.Expires.IsZero() {
			credential.Expires = token.Expires.UTC().Format(time.RFC3339)
		}
		if err := store.SetCredential(id, credential); err != nil {
			return err
		}
		a.announce(ctx, id)
		return nil
	}
}

func (a *authService) announce(ctx context.Context, provider string) {
	params, err := rpc.Marshal(rpc.AccountLoginCompletedNotification{Success: true, Provider: provider})
	if err == nil {
		err = rpcserver.Notify(ctx, "account/login/completed", params)
	}
	if err != nil {
		a.log.Debug("auth: login completion notice not delivered", "provider", provider, "error", err)
	}
}

func (a *authService) begin(ctx context.Context, entry authEntry, flow string) (*authTransaction, error) {
	tx := a.newTransaction(ctx, entry, flow)
	tx.release = context.AfterFunc(ctx, func() { a.drop(tx.id) })
	if err := a.admit(tx); err != nil {
		tx.release()
		tx.stop()
		return nil, err
	}
	if flow != authFlowAPIKey {
		go a.run(tx, entry.workers[flow])
	}
	return tx, nil
}

func (a *authService) newTransaction(ctx context.Context, entry authEntry, flow string) *authTransaction {
	now := a.now()
	lifetime := authWindows[flow]
	if flow != authFlowAPIKey {
		lifetime += authSetupMargin
	}
	work, stop := context.WithTimeout(ctx, lifetime)
	tx := &authTransaction{
		id: "auth-" + rand.Text(), connection: rpcserver.ConnectionID(ctx), key: entry.key, flow: flow,
		label: entry.descriptor.DisplayName, target: entry.descriptor.Target, created: now, expires: now.Add(lifetime),
		saveKey: entry.saveKey, now: a.now, pasted: make(chan string, 1), ready: make(chan struct{}),
		work: work, stop: stop, release: func() bool { return false }, state: authWaiting,
	}
	if flow == authFlowAPIKey {
		tx.state = authAwaitingInput
		tx.challenge = &authChallenge{Kind: authFlowAPIKey, Prompt: "Paste the " + tx.label + " API key", ExpiresAt: formatAuthTime(tx.expires)}
		tx.signal()
	}
	context.AfterFunc(work, tx.lapse)
	return tx
}

func (a *authService) run(tx *authTransaction, work authWork) {
	defer tx.stop()
	tx.finish(work(tx.work, tx))
}

func (a *authService) admit(tx *authTransaction) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	var mine []*authTransaction
	for _, other := range a.transactions {
		if other.key == tx.key && other.live() {
			return nativeFailure(rpc.ErrorBusy, "a login for "+tx.label+" is already in progress; cancel it before you start another")
		}
		if other.connection == tx.connection {
			mine = append(mine, other)
		}
	}
	if err := a.evict(mine); err != nil {
		return err
	}
	a.transactions[tx.id] = tx
	return nil
}

func (a *authService) evict(mine []*authTransaction) error {
	active := 0
	for _, tx := range mine {
		if tx.live() {
			active++
		}
	}
	if active >= authConnectionLimit {
		return nativeFailure(rpc.ErrorLimitExceeded, "this connection already runs 4 logins; cancel one before you start another")
	}
	finished := slices.DeleteFunc(mine, (*authTransaction).live)
	slices.SortFunc(finished, func(x, y *authTransaction) int { return x.created.Compare(y.created) })
	for _, tx := range finished[:max(0, len(finished)+active+1-authConnectionLimit)] {
		delete(a.transactions, tx.id)
		tx.release()
		tx.stop()
	}
	return nil
}

func (a *authService) drop(id string) {
	a.mu.Lock()
	defer a.mu.Unlock()
	delete(a.transactions, id)
}

func (a *authService) find(ctx context.Context, id string) (*authTransaction, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	tx, ok := a.transactions[id]
	if !ok || tx.connection != rpcserver.ConnectionID(ctx) {
		return nil, nativeFailure(rpc.ErrorNotFound, "the login transaction is not open on this connection; start a new login")
	}
	return tx, nil
}

func (e authEntry) flows() []string {
	flows := []string{}
	for _, flow := range authFlowOrder {
		_, worker := e.workers[flow]
		if worker || (flow == authFlowAPIKey && e.saveKey != nil) {
			flows = append(flows, flow)
		}
	}
	return flows
}

func (e authEntry) offers(flow string) error {
	if slices.Contains(e.descriptor.Flows, flow) {
		return nil
	}
	if e.descriptor.UnavailableReason != "" {
		return invalidNative(e.descriptor.DisplayName + " login is unavailable: " + e.descriptor.UnavailableReason)
	}
	return invalidNative(e.descriptor.DisplayName + " does not offer the " + flow + " flow here; choose one of: " + strings.Join(e.descriptor.Flows, ", "))
}

func (tx *authTransaction) submitKey(response authResponse) (authStatus, error) {
	if response.Kind != authFlowAPIKey {
		return authStatus{}, invalidNative("an apiKey login accepts only response.kind apiKey")
	}
	if err := checkAuthText(response.Value, apiKeyLimit); err != nil {
		return authStatus{}, err
	}
	if err := tx.claim(authAwaitingInput); err != nil {
		return authStatus{}, err
	}
	tx.finish(tx.saveKey(tx.work, response.Value))
	tx.stop()
	return tx.snapshot(), nil
}

func (tx *authTransaction) submitPaste(response authResponse) (authStatus, error) {
	if err := checkAuthText(response.Value, pastedLimit); err != nil {
		return authStatus{}, err
	}
	if err := tx.checkPasted(response); err != nil {
		return authStatus{}, err
	}
	if err := tx.claim(authWaiting); err != nil {
		return authStatus{}, err
	}
	tx.pasted <- response.Value
	return tx.snapshot(), nil
}

func (tx *authTransaction) checkPasted(response authResponse) error {
	tx.mu.Lock()
	advertised := tx.advertised
	tx.mu.Unlock()
	if advertised == nil {
		return invalidNative("the login has no authorization request yet; wait for its challenge")
	}
	query := advertised.Query()
	switch response.Kind {
	case "redirectUrl":
		return checkRedirect(response.Value, query.Get("redirect_uri"), query.Get("state"))
	case "authorizationCode":
		return checkCode(response.Value, query.Get("state"))
	}
	return invalidNative("a manualCode login accepts response.kind redirectUrl or authorizationCode")
}

func (tx *authTransaction) claim(expected string) error {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	switch tx.state {
	case expected:
		tx.state = authExchanging
		return nil
	case authExpired:
		return nativeFailure(rpc.ErrorExpired, "the login expired; start a new login")
	}
	return invalidNative("the login is " + tx.state + " and accepts no response; start a new login")
}

func (tx *authTransaction) cancelRequest() string {
	tx.mu.Lock()
	if !liveState(tx.state) {
		tx.mu.Unlock()
		return "completed"
	}
	if tx.state != authExchanging {
		tx.state = authCancelled
		tx.signal()
	}
	tx.mu.Unlock()
	tx.stop()
	return "cancelled"
}

func (tx *authTransaction) prompt() oauthPrompt {
	if tx.flow == authFlowManualCode {
		return oauthPrompt{open: tx.open, paste: tx.paste}
	}
	return oauthPrompt{open: tx.open}
}

func (tx *authTransaction) open(link string) error {
	advertised, err := url.Parse(link)
	if err != nil {
		return fmt.Errorf("parse authorization URL: %w", err)
	}
	challenge := &authChallenge{Kind: tx.flow, AuthorizationURL: link}
	if tx.flow == authFlowManualCode {
		challenge.Prompt = "Paste the redirect URL or the authorization code"
	}
	tx.present(challenge, advertised)
	return nil
}

func (tx *authTransaction) show(code, verification string) {
	tx.present(&authChallenge{Kind: tx.flow, VerificationURL: verification, UserCode: code}, nil)
}

func (tx *authTransaction) present(challenge *authChallenge, advertised *url.URL) {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	if !liveState(tx.state) {
		return
	}
	tx.challengeEnd = tx.now().Add(authWindows[tx.flow])
	challenge.ExpiresAt = formatAuthTime(tx.challengeEnd)
	tx.challenge, tx.advertised, tx.state = challenge, advertised, authWaiting
	tx.signal()
}

func (tx *authTransaction) paste(ctx context.Context) (string, error) {
	select {
	case value := <-tx.pasted:
		return value, nil
	case <-ctx.Done():
		return "", ctx.Err()
	}
}

func (tx *authTransaction) finish(err error) {
	state, failure := tx.outcome(err)
	tx.mu.Lock()
	defer tx.mu.Unlock()
	if err != nil && !liveState(tx.state) {
		return
	}
	tx.state, tx.failure = state, failure
	tx.signal()
}

func (tx *authTransaction) outcome(err error) (string, string) {
	switch {
	case err == nil:
		return authSucceeded, ""
	case tx.lapsed(err):
		return authExpired, "the login expired before it finished; start a new login"
	case tx.work.Err() != nil:
		return authCancelled, ""
	}
	return authFailed, err.Error()
}

func (tx *authTransaction) lapsed(err error) bool {
	if errors.Is(err, context.DeadlineExceeded) || errors.Is(tx.work.Err(), context.DeadlineExceeded) {
		return true
	}
	tx.mu.Lock()
	defer tx.mu.Unlock()
	return tx.challenge != nil && !tx.now().Before(tx.challengeEnd)
}

func (tx *authTransaction) lapse() {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	if tx.state != authAwaitingInput {
		return
	}
	tx.state = authCancelled
	if errors.Is(tx.work.Err(), context.DeadlineExceeded) {
		tx.state = authExpired
	}
	tx.signal()
}

func (tx *authTransaction) signal() {
	if !tx.signalled {
		tx.signalled = true
		close(tx.ready)
	}
}

func (tx *authTransaction) live() bool {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	return liveState(tx.state)
}

func (tx *authTransaction) await(ctx context.Context) authStatus {
	select {
	case <-tx.ready:
	case <-ctx.Done():
	}
	return tx.snapshot()
}

func (tx *authTransaction) snapshot() authStatus {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	status := authStatus{TransactionID: tx.id, Target: tx.target, State: tx.state, Error: tx.failure, ExpiresAt: formatAuthTime(tx.expires)}
	if tx.challenge != nil && liveState(tx.state) {
		challenge := *tx.challenge
		status.Challenge = &challenge
	}
	return status
}

func newAuthEntry(target authTarget, key, displayName string) authEntry {
	return authEntry{
		descriptor: authDescriptor{Target: target, DisplayName: displayName, Flows: []string{}},
		key:        key,
		workers:    map[string]authWork{},
	}
}

func providerTargetIDs() []string {
	return slices.Sorted(slices.Values(slices.Concat(auth.Providers(), auth.RunnerServices())))
}

func providerAccount(store *auth.Store, id string) (bool, string) {
	for _, entry := range store.Entries() {
		if entry.ID == id {
			return true, entry.Credential.Account
		}
	}
	return false, ""
}

func checkCallbackMode(flow string, mode jsontext.Value) error {
	required, callback := authCallbackModes[flow]
	if !callback && flow != authFlowAPIKey && flow != authFlowCursorDeepLink {
		return invalidNative("flow must be one of: " + strings.Join(authFlowOrder, ", "))
	}
	if len(mode) == 0 {
		if callback {
			return invalidNative("flow " + flow + " requires callbackMode " + required)
		}
		return nil
	}
	return checkCallbackValue(flow, mode, callback, required)
}

func checkCallbackValue(flow string, mode jsontext.Value, callback bool, required string) error {
	if !callback {
		return invalidNative("flow " + flow + " takes no callbackMode; omit it")
	}
	if mode.Kind() != '"' {
		return invalidNative("callbackMode must be the string " + required + " for flow " + flow)
	}
	var got string
	if err := json.Unmarshal(mode, &got); err != nil || got != required {
		return invalidNative("flow " + flow + " requires callbackMode " + required)
	}
	return nil
}

func checkAuthText(value string, limit int) error {
	policy := inputtext.Policy{Lines: inputtext.SingleLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: limit}}
	if err := inputtext.CheckRequest([]inputtext.Item{{Field: "response.value", Policy: policy, Text: value}}); err != nil {
		return nativeFailure(rpc.ErrorInvalidParams, err.Error())
	}
	if value == "" {
		return invalidNative("response.value is empty; paste the full value")
	}
	return nil
}

func checkRedirect(value, redirect, state string) error {
	got, err := url.Parse(value)
	want, wantErr := url.Parse(redirect)
	if err != nil || wantErr != nil || got.Scheme != want.Scheme || got.Host != want.Host || got.Path != want.Path {
		return invalidNative("response.value is not the advertised redirect; paste the full address that the browser opened")
	}
	query := got.Query()
	if query.Get("code") == "" {
		return invalidNative("the redirect holds no authorization code; paste the full address that the browser opened")
	}
	if query.Get("state") != state {
		return invalidNative("the redirect state does not match this login; paste the redirect from this login")
	}
	return nil
}

func checkCode(value, state string) error {
	code, pastedState, found := strings.Cut(value, "#")
	if code == "" {
		return invalidNative("the authorization code is empty; paste the code that the provider showed")
	}
	if found && pastedState != state {
		return invalidNative("the authorization code state does not match this login; paste the code from this login")
	}
	return nil
}

func liveState(state string) bool {
	return state == authAwaitingInput || state == authWaiting || state == authExchanging
}

func formatAuthTime(at time.Time) string { return at.UTC().Format(authTimeLayout) }

func decodeNative[T any](params jsontext.Value) (T, error) {
	var p T
	if len(params) == 0 {
		return p, nil
	}
	if err := json.Unmarshal(params, &p); err != nil {
		return p, invalidNative("invalid params: " + err.Error())
	}
	return p, nil
}

func encodeNative(v any) (jsontext.Value, error) {
	raw, err := json.Marshal(v)
	if err != nil {
		return nil, fmt.Errorf("encode authentication result: %w", err)
	}
	return raw, nil
}

func invalidNative(message string) error { return nativeFailure(rpc.ErrorInvalidParams, message) }

func nativeFailure(kind rpc.ErrorKind, message string) error {
	native := rpc.NativeKind(kind)
	return &rpc.RPCError{Code: native.Code, Message: message, Data: native.Data}
}
