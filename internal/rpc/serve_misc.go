package rpc

import (
	"cmp"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	"os"
	"runtime"
	"slices"
	"time"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/config"
)

func platform() (family, name string) {
	switch runtime.GOOS {
	case "windows":
		return "windows", "windows"
	case "darwin":
		return "unix", "macos"
	}
	return "unix", runtime.GOOS
}

var chatGPTProvider = func() (oauth.Provider, bool) { return oauth.ProviderByID("chatgpt") }

func (s *server) initialize(context.Context, jsontext.Value) (jsontext.Value, error) {
	cfg := s.host.Config()
	family, name := platform()
	o := newOut()
	o.open()
	o.str("userAgent", string(cfg.Product)+"/"+version())
	o.str("codexHome", cfg.Home)
	o.str("platformFamily", family)
	o.str("platformOs", name)
	o.close()
	return o.bytes()
}

func (s *server) modelList(ctx context.Context, _ jsontext.Value) (jsontext.Value, error) {
	models, err := s.host.Models(ctx)
	if err != nil {
		return nil, internalError(err)
	}
	current := s.currentConfig().Model
	response := ModelListResponse{Data: make([]Model, 0, len(models))}
	for _, m := range models {
		efforts := make([]ReasoningEffortOption, 0, len(m.Efforts))
		for _, effort := range m.Efforts {
			efforts = append(efforts, ReasoningEffortOption{ReasoningEffort: effort, Description: effort})
		}
		response.Data = append(response.Data, Model{
			ID: m.ID, Model: m.ID, DisplayName: m.ID, IsDefault: m.ID == current,
			DefaultReasoningEffort: defaultEffort(m.Efforts), SupportedReasoningEfforts: efforts,
		})
	}
	return encode(response)
}

func defaultEffort(efforts []string) string {
	switch {
	case slices.Contains(efforts, "medium"):
		return "medium"
	case len(efforts) > 0:
		return efforts[0]
	}
	return "none"
}

func (s *server) configRead(context.Context, jsontext.Value) (jsontext.Value, error) {
	cfg := s.currentConfig()
	return encode(ConfigReadResponse{
		Config: ConfigValues{
			Model: cfg.Model, ModelProvider: provider(cfg.Model), ApprovalPolicy: policyName(string(cfg.Approval)),
			SandboxMode: sandboxModeName(string(cfg.Sandbox)), ModelReasoningEffort: effortName(cfg.Reasoning),
		},
		Origins: map[string]string{},
	})
}

func settingKey(keyPath string, value any) (string, any, error) {
	switch keyPath {
	case "approval_policy", "sandbox_mode", "model_reasoning_effort":
		return mappedSettingKey(keyPath, value)
	case "model", "approval", "sandbox", "reasoning":
		return directSettingKey(keyPath, value)
	}
	return "", nil, invalidParams("unsupported keyPath " + keyPath)
}

func mappedSettingKey(keyPath string, value any) (string, any, error) {
	text, ok := value.(string)
	if !ok {
		return "", nil, invalidParams(keyPath + " must be a string")
	}
	switch keyPath {
	case "approval_policy":
		mode, err := approvalMode(text)
		return "approval", string(mode), err
	case "sandbox_mode":
		mode, err := sandboxMode(text)
		return "sandbox", string(mode), err
	}
	mode, err := effortMode(text)
	return "reasoning", string(mode), err
}

func directSettingKey(keyPath string, value any) (string, any, error) {
	text, ok := value.(string)
	if !ok {
		return "", nil, invalidParams(keyPath + " must be a string")
	}
	switch keyPath {
	case "approval":
		mode, err := approvalMode(text)
		return "approval", string(mode), err
	case "sandbox":
		mode, err := sandboxMode(text)
		return "sandbox", string(mode), err
	case "reasoning":
		mode, err := effortMode(text)
		return "reasoning", string(mode), err
	}
	return "model", text, nil
}

func (s *server) configValueWrite(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	var keyPath string
	var value any
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "keyPath":
			keyPath, err = readString(d)
		case "value":
			value, err = readPlain(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
	if err != nil {
		return nil, err
	}
	key, setting, err := settingKey(keyPath, value)
	if err != nil {
		return nil, err
	}
	cfg := s.host.Config()
	if writeErr := config.WriteSetting(cfg.Home, key, setting); writeErr != nil {
		return nil, invalidParams(writeErr.Error())
	}
	overridesOf(s.host).set(key, setting)
	path := cfg.SettingsPath()
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, internalError(err)
	}
	sum := sha256.Sum256(data)
	return encode(ConfigWriteResponse{FilePath: path, Status: "ok", Version: hex.EncodeToString(sum[:])})
}

func (s *server) authStore() *auth.Store { return s.host.Auth() }

func (s *server) credentials() (*auth.Store, error) {
	store := s.authStore()
	if store == nil {
		return nil, invalidRequest("the credential store is not available")
	}
	return store, nil
}

func (s *server) accountRead(context.Context, jsontext.Value) (jsontext.Value, error) {
	response := AccountReadResponse{}
	var entries []auth.ProviderEntry
	if store := s.authStore(); store != nil {
		entries = store.Entries()
	}
	for _, entry := range entries {
		if entry.ID == "chatgpt" && entry.Credential.Type == "oauth" {
			response.Account = &Account{Type: "chatgpt", Email: entry.Credential.Account, PlanType: "unknown"}
			break
		}
		if response.Account == nil {
			response.Account = &Account{Type: "apiKey"}
		}
	}
	return encode(response)
}

func newLoginID() string { return "login-" + rand.Text() }

func (s *server) loginStart(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p AccountLoginStartParams
	fields := map[string]*string{"type": &p.Type, "apiKey": &p.APIKey, "provider": &p.Provider, "method": &p.Method}
	if err := decodeParams(params, stringMembers(fields)); err != nil {
		return nil, err
	}
	if _, err := s.credentials(); err != nil {
		return nil, err
	}
	switch loginFlow(p) {
	case "apiKey":
		return s.loginKey(p)
	case "chatgpt":
		return s.loginBrowser(ctx)
	case "chatgptDeviceCode":
		return s.loginDevice(ctx)
	}
	name := cmp.Or(p.Provider, p.Type)
	if name == "" {
		return nil, invalidParams("account/login/start needs a type or a provider")
	}
	return nil, invalidParams("login for " + name + " runs in the terminal: dal login " + name)
}

func loginFlow(p AccountLoginStartParams) string {
	if p.APIKey != "" {
		return "apiKey"
	}
	if p.Type == "" && p.Provider == "chatgpt" {
		if p.Method == "device" {
			return "chatgptDeviceCode"
		}
		return "chatgpt"
	}
	return p.Type
}

func (s *server) loginKey(p AccountLoginStartParams) (jsontext.Value, error) {
	name := cmp.Or(p.Provider, "openai")
	if p.APIKey == "" {
		return nil, invalidParams("provider " + name + " needs an apiKey")
	}
	store, err := s.credentials()
	if err != nil {
		return nil, err
	}
	if err := store.SetAPIKey(name, p.APIKey); err != nil {
		return nil, invalidParams(err.Error())
	}
	return encode(LoginAccountResponse{Type: "apiKey"})
}

func (s *server) startLogin(ctx context.Context, flow func(context.Context) (oauth.Token, error)) (string, error) {
	id := newLoginID()
	loginCtx, cancel := context.WithCancel(ctx)
	started := s.spawn(func() { s.logins[id] = cancel }, func() {
		token, err := flow(loginCtx)
		s.finishLogin(id, token, err)
	})
	if !started {
		cancel()
		return "", invalidRequest("the connection is closing")
	}
	return id, nil
}

func (s *server) finishLogin(id string, token oauth.Token, err error) {
	s.mu.Lock()
	cancel := s.logins[id]
	delete(s.logins, id)
	s.mu.Unlock()
	if cancel != nil {
		cancel()
	}
	if err == nil {
		err = s.authStore().SetCredential("chatgpt", auth.Credential{
			Type: "oauth", Access: token.Access, Refresh: token.Refresh, Expires: token.Expires.UTC().Format(time.RFC3339), Account: token.Account,
		})
	}
	note := AccountLoginCompletedNotification{LoginID: id, Success: err == nil}
	if err != nil {
		note.Error = err.Error()
	}
	s.send("account/login/completed", note)
}

func (s *server) loginBrowser(ctx context.Context) (jsontext.Value, error) {
	provider, ok := chatGPTProvider()
	if !ok {
		return nil, invalidParams("chatgpt login is not available")
	}
	urls := make(chan string, 1)
	id, err := s.startLogin(ctx, func(ctx context.Context) (oauth.Token, error) {
		defer close(urls)
		return oauth.PKCE(ctx, provider, func(url string) error {
			select {
			case urls <- url:
			default:
			}
			return nil
		}, nil)
	})
	if err != nil {
		return nil, err
	}
	url, ok := <-urls
	if !ok {
		return nil, invalidRequest("chatgpt login failed before it produced an authorization URL")
	}
	return encode(LoginAccountResponse{Type: "chatgpt", LoginID: id, AuthURL: url})
}

func (s *server) loginDevice(ctx context.Context) (jsontext.Value, error) {
	provider, ok := chatGPTProvider()
	if !ok {
		return nil, invalidParams("chatgpt login is not available")
	}
	codes := make(chan [2]string, 1)
	id, err := s.startLogin(ctx, func(ctx context.Context) (oauth.Token, error) {
		defer close(codes)
		return oauth.DeviceCode(ctx, provider, func(code, url string) {
			select {
			case codes <- [2]string{url, code}:
			default:
			}
		})
	})
	if err != nil {
		return nil, err
	}
	code, ok := <-codes
	if !ok {
		return nil, invalidRequest("chatgpt device login failed before it produced a code")
	}
	return encode(LoginAccountResponse{Type: "chatgptDeviceCode", LoginID: id, VerificationURL: code[0], UserCode: code[1]})
}

func (s *server) loginCancel(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	var id string
	if err := decodeParams(params, stringMembers(map[string]*string{"loginId": &id})); err != nil {
		return nil, err
	}
	s.mu.Lock()
	cancel, ok := s.logins[id]
	s.mu.Unlock()
	status := "notFound"
	if ok {
		cancel()
		status = "canceled"
	}
	return encode(CancelLoginAccountResponse{Status: status})
}

func (s *server) logout(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	var name string
	if err := decodeParams(params, stringMembers(map[string]*string{"provider": &name})); err != nil {
		return nil, err
	}
	store, err := s.credentials()
	if err != nil {
		return nil, err
	}
	if name == "" {
		entries := store.Entries()
		if len(entries) != 1 {
			return nil, invalidParams("account/logout needs a provider")
		}
		name = entries[0].ID
	}
	if err := store.Logout(name); err != nil {
		return nil, invalidParams(err.Error())
	}
	return emptyObject(), nil
}

func (s *server) skillsList(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	var cwds []string
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		if name != "cwds" {
			return d.SkipValue()
		}
		return readArray(d, func() error {
			cwd, err := readString(d)
			cwds = append(cwds, cwd)
			return err
		})
	})
	if err != nil {
		return nil, err
	}
	if len(cwds) == 0 {
		cwds = []string{s.host.Config().Cwd}
	}
	response := SkillsListResponse{Data: []SkillsListEntry{}}
	for _, cwd := range cwds {
		skills, err := s.host.Skills(cwd)
		if err != nil {
			return nil, invalidRequest(err.Error())
		}
		if len(skills) == 0 {
			continue
		}
		entry := SkillsListEntry{Cwd: cwd, Skills: make([]SkillMetadata, 0, len(skills))}
		for _, skill := range skills {
			entry.Skills = append(entry.Skills, SkillMetadata{Name: skill.Name, Description: skill.Description, Path: skill.Path, Scope: "user", Enabled: true})
		}
		response.Data = append(response.Data, entry)
	}
	return encode(response)
}

func (s *server) keywordsList(context.Context, jsontext.Value) (jsontext.Value, error) {
	table := s.host.Keywords()
	list := make(keywordList, 0, len(table))
	for _, keyword := range table {
		if keyword.Enabled {
			list = append(list, Keyword{Word: keyword.Word, Hue: keyword.Hue})
		}
	}
	return encode(list)
}
