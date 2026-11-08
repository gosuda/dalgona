package auth

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"maps"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"sync"
	"syscall"
)

var ErrUnknownProvider = errors.New("auth: unknown provider")

type Product string

const (
	ProductDal     Product = "dal"
	ProductDalgona Product = "dalgona"
)

type (
	ProductScope = Product
	ProviderDef  struct {
		ID, EnvKey string
		Products   []Product
	}
)

var providerDefs = map[string]ProviderDef{
	"openai":     {ID: "openai", EnvKey: "OPENAI_API_KEY", Products: []Product{ProductDal, ProductDalgona}},
	"anthropic":  {ID: "anthropic", EnvKey: "ANTHROPIC_API_KEY", Products: []Product{ProductDal, ProductDalgona}},
	"google":     {ID: "google", EnvKey: "GEMINI_API_KEY", Products: []Product{ProductDal, ProductDalgona}},
	"openrouter": {ID: "openrouter", EnvKey: "OPENROUTER_API_KEY", Products: []Product{ProductDal, ProductDalgona}},
	"devin":      {ID: "devin", EnvKey: "DEVIN_API_KEY", Products: []Product{ProductDal, ProductDalgona}},
	"sandbox0":   {ID: "sandbox0", EnvKey: "SANDBOX0_TOKEN", Products: []Product{ProductDalgona}},
}

func ProviderDefFor(id string) (ProviderDef, bool) {
	definition, ok := providerDefs[id]
	return definition, ok
}

type (
	Credential struct {
		Type    string `json:"type"`
		Key     string `json:"key,omitempty"`
		Access  string `json:"access,omitempty"`
		Refresh string `json:"refresh,omitempty"`
		Expires string `json:"expires,omitempty"`
		Account string `json:"account,omitempty"`
	}
	Store struct {
		mu           sync.Mutex
		dir          string
		data         map[string]Credential
		raw          map[string]jsontext.Value
		authInfo     os.FileInfo
		env          func(string) string
		secrets      map[string]string
		patterns     []pattern
		maskErr      error
		secretsDirty bool
	}
)

func Load(home string) (*Store, error) { return NewWithPatterns(home, nil) }

func New(home string) (*Store, error) { return NewWithPatterns(home, nil) }

func NewWithPatterns(home string, patterns []string) (*Store, error) {
	store := &Store{
		dir: home, data: map[string]Credential{}, raw: map[string]jsontext.Value{},
		env: os.Getenv, secrets: map[string]string{},
	}
	if err := store.loadAuth(); err != nil {
		return nil, err
	}
	if err := store.loadSecrets(); err != nil {
		return nil, err
	}
	compiled, err := compilePatterns(patterns)
	if err != nil {
		return nil, err
	}
	store.patterns = compiled
	return store, nil
}

func (s *Store) authPath() string { return filepath.Join(s.dir, "auth.json") }

func (s *Store) loadAuth() error {
	info, raw, err := readSecureInfo(s.authPath())
	if err != nil {
		return err
	}
	data, rawEntries := map[string]Credential{}, map[string]jsontext.Value{}
	if raw != nil {
		err = readObject(raw, func(provider string, value jsontext.Value) error {
			credential, decodeErr := decodeCredential(value)
			if decodeErr != nil {
				return fmt.Errorf("provider %q: %w", provider, decodeErr)
			}
			data[provider] = credential
			rawEntries[provider] = value
			return nil
		})
	}
	if err != nil {
		return fmt.Errorf("auth.json: %w", err)
	}
	s.data, s.raw, s.authInfo = data, rawEntries, info
	return nil
}

func (s *Store) reloadAuthIfChanged() error {
	info, err := os.Lstat(s.authPath())
	if errors.Is(err, os.ErrNotExist) || errors.Is(err, syscall.ENOTDIR) {
		info = nil
	} else if err != nil {
		return fmt.Errorf("inspect auth.json: %w", err)
	}
	if sameAuthFile(s.authInfo, info) {
		return nil
	}
	return s.loadAuth()
}

func sameAuthFile(previous, current os.FileInfo) bool {
	if previous == nil || current == nil {
		return previous == nil && current == nil
	}
	return os.SameFile(previous, current) && previous.ModTime().Equal(current.ModTime()) && previous.Size() == current.Size()
}

func openAuthLock(dir string) (*os.File, error) {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("create credential directory: %w", err)
	}
	file, err := os.OpenFile(filepath.Join(dir, "auth.lock"), os.O_RDWR|os.O_CREATE, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open auth.lock: %w", err)
	}
	return file, nil
}

func (s *Store) updateAuth(apply func()) error {
	unlock, err := lockAuth(s.dir)
	if err != nil {
		return err
	}
	err = s.rewriteAuth(apply)
	if err != nil {
		if loadErr := s.loadAuth(); loadErr != nil {
			s.authInfo = nil
		}
	}
	return errors.Join(err, unlock())
}

func (s *Store) rewriteAuth(apply func()) error {
	if err := s.loadAuth(); err != nil {
		return err
	}
	apply()
	if err := writeSecure(s.authPath(), s.encodeAuth); err != nil {
		return err
	}
	info, err := os.Lstat(s.authPath())
	if err != nil {
		return fmt.Errorf("inspect auth.json: %w", err)
	}
	s.authInfo = info
	return nil
}

func (s *Store) loadSecrets() error {
	raw, err := readSecure(filepath.Join(s.dir, "secrets.json"))
	if err != nil || raw == nil {
		return err
	}
	err = readObject(raw, func(placeholder string, value jsontext.Value) error {
		secret, decodeErr := jsonString(value)
		if decodeErr != nil {
			return fmt.Errorf("placeholder %q: %w", placeholder, decodeErr)
		}
		s.secrets[placeholder] = secret
		return nil
	})
	if err != nil {
		return fmt.Errorf("secrets.json: %w", err)
	}
	return nil
}

func readObject(raw []byte, visit func(string, jsontext.Value) error) error {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return fmt.Errorf("read object start: %w", err)
	}
	if start.Kind() != jsontext.KindBeginObject {
		return errors.New("expected a JSON object")
	}
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := decoder.ReadToken()
		if err != nil {
			return fmt.Errorf("read object name: %w", err)
		}
		provider := name.String()
		value, err := decoder.ReadValue()
		if err != nil {
			return fmt.Errorf("read object value: %w", err)
		}
		value = bytes.Clone(value)
		if err := visit(provider, value); err != nil {
			return err
		}
	}
	if _, err := decoder.ReadToken(); err != nil {
		return fmt.Errorf("read object end: %w", err)
	}
	if _, err := decoder.ReadToken(); !errors.Is(err, io.EOF) {
		return errors.New("unexpected data after JSON object")
	}
	return nil
}

func decodeCredential(raw jsontext.Value) (Credential, error) {
	var credential Credential
	err := readObject(raw, func(name string, value jsontext.Value) error {
		switch name {
		case "type", "key", "access", "refresh", "expires", "account":
			text, err := jsonString(value)
			if err != nil {
				return fmt.Errorf("field %q: %w", name, err)
			}
			setCredentialField(&credential, name, text)
		}
		return nil
	})
	return credential, err
}

func setCredentialField(credential *Credential, name, value string) {
	switch name {
	case "type":
		credential.Type = value
	case "key":
		credential.Key = value
	case "access":
		credential.Access = value
	case "refresh":
		credential.Refresh = value
	case "expires":
		credential.Expires = value
	case "account":
		credential.Account = value
	}
}

func jsonString(raw jsontext.Value) (string, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", fmt.Errorf("read string: %w", err)
	}
	if token.Kind() != jsontext.KindString {
		return "", errors.New("expected a JSON string")
	}
	text := token.String()
	if _, err := decoder.ReadToken(); !errors.Is(err, io.EOF) {
		return "", errors.New("unexpected data after JSON string")
	}
	return text, nil
}

func readSecure(path string) ([]byte, error) {
	_, raw, err := readSecureInfo(path)
	return raw, err
}

func readSecureInfo(path string) (os.FileInfo, []byte, error) {
	info, err := os.Lstat(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil, nil
	}
	if err != nil {
		return nil, nil, fmt.Errorf("inspect %s: %w", filepath.Base(path), err)
	}
	insecure := info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular()
	if runtime.GOOS != "windows" && info.Mode().Perm()&0o077 != 0 {
		insecure = true
	}
	if insecure {
		return nil, nil, fmt.Errorf("%s must be a regular file with mode 0600", filepath.Base(path))
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, nil, fmt.Errorf("read %s: %w", filepath.Base(path), err)
	}
	return info, raw, nil
}

func (s *Store) Token(ctx context.Context, provider string) (Credential, error) {
	if err := ctx.Err(); err != nil {
		return Credential{}, fmt.Errorf("read %s credential: %w", provider, err)
	}
	if key := envKey(provider, ""); key != "" {
		if value := s.env(key); value != "" {
			return Credential{Type: "api_key", Key: value}, nil
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.reloadAuthIfChanged(); err != nil {
		return Credential{}, fmt.Errorf("read %s credential: %w", provider, err)
	}
	credential, ok := s.data[provider]
	if !ok {
		return Credential{}, fmt.Errorf("%w %q", ErrUnknownProvider, provider)
	}
	return credential, nil
}

func (s *Store) TokenWithEnv(ctx context.Context, provider, apiEnv string) (Credential, error) {
	if apiEnv != "" {
		if value := s.env(apiEnv); value != "" {
			return Credential{Type: "api_key", Key: value}, nil
		}
	}
	return s.Token(ctx, provider)
}

func envKey(provider, custom string) string {
	if custom != "" {
		return custom
	}
	if definition, ok := providerDefs[provider]; ok {
		return definition.EnvKey
	}
	return strings.ToUpper(strings.NewReplacer("-", "_", ".", "_").Replace(provider)) + "_API_KEY"
}

func (s *Store) SetAPIKey(provider, key string) error {
	return s.SetCredential(provider, Credential{Type: "api_key", Key: key})
}

func (s *Store) SetCredential(provider string, c Credential) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	err := s.updateAuth(func() {
		s.data[provider] = c
		delete(s.raw, provider)
	})
	if err != nil {
		return fmt.Errorf("store %s credential: %w", provider, err)
	}
	return nil
}

func (s *Store) Logout(provider string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	err := s.updateAuth(func() {
		delete(s.data, provider)
		delete(s.raw, provider)
	})
	if err != nil {
		return fmt.Errorf("remove %s credential: %w", provider, err)
	}
	return nil
}

type ProviderEntry struct {
	ID         string
	Credential Credential
}

func (s *Store) Entries() []ProviderEntry {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.reloadForMask()
	entries := make([]ProviderEntry, 0, len(s.data))
	for _, id := range sortedCredentialIDs(s.data) {
		entries = append(entries, ProviderEntry{ID: id, Credential: s.data[id]})
	}
	return entries
}

func sortedCredentialIDs(credentials map[string]Credential) []string {
	return slices.Sorted(maps.Keys(credentials))
}

func (s *Store) encodeAuth(encoder *jsontext.Encoder) error {
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return fmt.Errorf("write auth object: %w", err)
	}
	for _, provider := range sortedCredentialIDs(s.data) {
		if err := encoder.WriteToken(jsontext.String(provider)); err != nil {
			return fmt.Errorf("write provider name: %w", err)
		}
		if err := writeAuthValue(encoder, s.raw[provider], s.data[provider]); err != nil {
			return fmt.Errorf("write provider %s: %w", provider, err)
		}
	}
	if err := encoder.WriteToken(jsontext.EndObject); err != nil {
		return fmt.Errorf("finish auth object: %w", err)
	}
	return nil
}

func writeAuthValue(encoder *jsontext.Encoder, raw jsontext.Value, credential Credential) error {
	if raw != nil {
		if err := encoder.WriteValue(raw); err != nil {
			return fmt.Errorf("write raw credential: %w", err)
		}
		return nil
	}
	if err := writeCredential(encoder, credential); err != nil {
		return fmt.Errorf("write typed credential: %w", err)
	}
	return nil
}

func writeCredential(encoder *jsontext.Encoder, credential Credential) error {
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return fmt.Errorf("write credential: %w", err)
	}
	fields := []struct{ name, value string }{
		{name: "type", value: credential.Type},
		{name: "key", value: credential.Key},
		{name: "access", value: credential.Access},
		{name: "refresh", value: credential.Refresh},
		{name: "expires", value: credential.Expires},
		{name: "account", value: credential.Account},
	}
	for _, field := range fields {
		if field.value == "" {
			continue
		}
		if err := encoder.WriteToken(jsontext.String(field.name)); err != nil {
			return fmt.Errorf("write credential field: %w", err)
		}
		if err := encoder.WriteToken(jsontext.String(field.value)); err != nil {
			return fmt.Errorf("write credential value: %w", err)
		}
	}
	if err := encoder.WriteToken(jsontext.EndObject); err != nil {
		return fmt.Errorf("finish credential: %w", err)
	}
	return nil
}

func writeSecure(path string, encode func(*jsontext.Encoder) error) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return fmt.Errorf("create credential directory: %w", err)
	}
	file, err := os.CreateTemp(filepath.Dir(path), ".credentials-*.tmp")
	if err != nil {
		return fmt.Errorf("create temporary credential file: %w", err)
	}
	temporary := file.Name()
	defer func() { _ = os.Remove(temporary) }()
	if err := file.Chmod(0o600); err != nil {
		return errors.Join(fmt.Errorf("secure temporary credential file: %w", err), file.Close())
	}
	if err := encode(jsontext.NewEncoder(file)); err != nil {
		return errors.Join(err, file.Close())
	}
	if err := file.Close(); err != nil {
		return fmt.Errorf("close credential file: %w", err)
	}
	if err := os.Rename(temporary, path); err != nil {
		return fmt.Errorf("replace %s: %w", filepath.Base(path), err)
	}
	return nil
}

func (s *Store) Dir() string { return s.dir }

type Masker interface {
	Mask(string) string
	Unmask(string) string
}

func (s *Store) Masker() Masker { return s }

func ParseKeyValue(value string) (string, string, bool) {
	return strings.Cut(value, "=")
}
