package github

import (
	"context"
	"crypto"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"
	"time"

	json "encoding/json/v2"
)

type InstallationConfig struct {
	AppID          int64
	PrivateKeyFile string
	Repositories   []Repository
}

type InstallationCredential struct {
	appID         int64
	pemBytes      []byte
	key           *rsa.PrivateKey
	repositories  map[Repository]bool
	baseURL       string
	client        *http.Client
	mu            sync.Mutex
	installations map[Repository]int64
	tokens        map[Repository]cachedToken
}

type cachedToken struct {
	token Token
	wait  chan struct{}
	err   error
}

type tokenRequest struct {
	Repositories []string         `json:"repositories"`
	Permissions  tokenPermissions `json:"permissions"`
}

type tokenPermissions struct {
	Issues       string `json:"issues"`
	PullRequests string `json:"pull_requests"`
	Contents     string `json:"contents"`
}

type jwtHeader struct {
	Alg string `json:"alg"`
	Typ string `json:"typ"`
}

type jwtClaims struct {
	IssuedAt  int64  `json:"iat"`
	ExpiresAt int64  `json:"exp"`
	Issuer    string `json:"iss"`
}

func NewInstallationCredential(ctx context.Context, cfg InstallationConfig, transport *http.Client) (*InstallationCredential, error) {
	if cfg.AppID <= 0 {
		return nil, errors.New("github: invalid app id")
	}
	if cfg.PrivateKeyFile == "" {
		return nil, errors.New("github: private key file is required")
	}
	repositories, err := parseInstallationRepositories(cfg.Repositories)
	if err != nil {
		return nil, err
	}
	key, pemBytes, err := loadInstallationKey(cfg.PrivateKeyFile)
	if err != nil {
		return nil, err
	}
	if transport == nil {
		transport = &http.Client{Timeout: 30 * time.Second}
	}
	return &InstallationCredential{
		appID:         cfg.AppID,
		pemBytes:      pemBytes,
		key:           key,
		repositories:  repositories,
		baseURL:       "https://api.github.com",
		client:        transport,
		installations: make(map[Repository]int64),
		tokens:        make(map[Repository]cachedToken),
	}, nil
}

func parseInstallationRepositories(repositories []Repository) (map[Repository]bool, error) {
	if len(repositories) == 0 {
		return nil, errors.New("github: at least one repository is required")
	}
	parsed := make(map[Repository]bool, len(repositories))
	for _, repo := range repositories {
		if repo.Owner == "" || repo.Name == "" {
			return nil, errors.New("github: invalid repository")
		}
		parsed[repo] = true
	}
	return parsed, nil
}

func loadInstallationKey(path string) (*rsa.PrivateKey, []byte, error) {
	pemBytes, err := readPrivateKeyFile(path)
	if err != nil {
		return nil, nil, err
	}
	key, err := parseRSAPrivateKey(pemBytes)
	if err != nil {
		return nil, nil, err
	}
	return key, pemBytes, nil
}

func parseRSAPrivateKey(pemBytes []byte) (*rsa.PrivateKey, error) {
	block, _ := pem.Decode(pemBytes)
	if block == nil {
		return nil, errors.New("github: invalid rsa private key")
	}
	var key *rsa.PrivateKey
	switch block.Type {
	case "RSA PRIVATE KEY":
		parsed, err := x509.ParsePKCS1PrivateKey(block.Bytes)
		if err != nil {
			return nil, errors.New("github: invalid rsa private key")
		}
		key = parsed
	case "PRIVATE KEY":
		parsed, err := x509.ParsePKCS8PrivateKey(block.Bytes)
		if err != nil {
			return nil, errors.New("github: invalid rsa private key")
		}
		rsaKey, ok := parsed.(*rsa.PrivateKey)
		if !ok {
			return nil, errors.New("github: invalid rsa private key")
		}
		key = rsaKey
	default:
		return nil, errors.New("github: invalid rsa private key")
	}
	if key.N.BitLen() < 2048 {
		return nil, errors.New("github: invalid rsa private key")
	}
	return key, nil
}

func (c *InstallationCredential) Token(ctx context.Context, request CredentialRequest) (Token, error) {
	if request.Workflow {
		return Token{}, errors.New("github: workflow permission requires a workflow-capable credential")
	}
	repo := request.Repository
	if !c.repositories[repo] {
		return Token{}, errors.New("github: repository is not allowlisted")
	}

	c.mu.Lock()
	if cached, ok := c.tokens[repo]; ok {
		if cached.wait != nil {
			wait := cached.wait
			c.mu.Unlock()
			return c.waitForToken(ctx, repo, wait)
		}
		if cached.err == nil && time.Until(cached.token.ExpiresAt) > 120*time.Second {
			token := cached.token
			c.mu.Unlock()
			return token, nil
		}
	}
	wait := make(chan struct{})
	c.tokens[repo] = cachedToken{wait: wait}
	c.mu.Unlock()

	token, err := c.fetchToken(ctx, repo)

	c.mu.Lock()
	if err != nil {
		c.tokens[repo] = cachedToken{err: err}
	} else {
		c.tokens[repo] = cachedToken{token: token}
	}
	close(wait)
	c.mu.Unlock()
	return token, err
}

func (c *InstallationCredential) waitForToken(ctx context.Context, repo Repository, wait chan struct{}) (Token, error) {
	select {
	case <-ctx.Done():
		return Token{}, ctx.Err()
	case <-wait:
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	finished := c.tokens[repo]
	return finished.token, finished.err
}

func (c *InstallationCredential) fetchToken(ctx context.Context, repo Repository) (Token, error) {
	installationID, err := c.installationID(ctx, repo)
	if err != nil {
		return Token{}, err
	}
	jwt, err := c.appJWT()
	if err != nil {
		return Token{}, err
	}
	req, err := c.newTokenExchangeRequest(ctx, installationID, jwt, repo)
	if err != nil {
		return Token{}, err
	}

	resp, err := c.client.Do(req)
	if err != nil {
		return Token{}, fmt.Errorf("github: exchange installation token: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode != http.StatusCreated && resp.StatusCode != http.StatusOK {
		return Token{}, fmt.Errorf("github: exchange installation token: status %d", resp.StatusCode)
	}
	var decoded struct {
		Token     string    `json:"token"`
		ExpiresAt time.Time `json:"expires_at"`
	}
	if err := decodeBounded(resp, &decoded, "github: decode token response"); err != nil {
		return Token{}, err
	}
	if decoded.Token == "" {
		return Token{}, errors.New("github: token response missing token")
	}
	return Token{Value: decoded.Token, ExpiresAt: decoded.ExpiresAt}, nil
}

func (c *InstallationCredential) newTokenExchangeRequest(ctx context.Context, installationID int64, jwt string, repo Repository) (*http.Request, error) {
	payload := tokenRequest{
		Repositories: []string{repo.Name},
		Permissions: tokenPermissions{
			Issues:       "write",
			PullRequests: "write",
			Contents:     "write",
		},
	}
	body, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("github: encode token request: %w", err)
	}
	endpoint := fmt.Sprintf("%s/app/installations/%d/access_tokens", c.baseURL, installationID)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, strings.NewReader(string(body)))
	if err != nil {
		return nil, fmt.Errorf("github: build token request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+jwt)
	req.Header.Set("Accept", "application/vnd.github+json")
	req.Header.Set("X-GitHub-Api-Version", "2026-03-10")
	req.Header.Set("Content-Type", "application/json")
	return req, nil
}

func (c *InstallationCredential) installationID(ctx context.Context, repo Repository) (int64, error) {
	c.mu.Lock()
	if id, ok := c.installations[repo]; ok {
		c.mu.Unlock()
		return id, nil
	}
	c.mu.Unlock()

	jwt, err := c.appJWT()
	if err != nil {
		return 0, err
	}
	url := fmt.Sprintf("%s/repos/%s/%s/installation", c.baseURL, repo.Owner, repo.Name)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return 0, fmt.Errorf("github: build installation request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+jwt)
	req.Header.Set("Accept", "application/vnd.github+json")
	req.Header.Set("X-GitHub-Api-Version", "2026-03-10")

	resp, err := c.client.Do(req)
	if err != nil {
		return 0, fmt.Errorf("github: discover installation: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode != http.StatusOK {
		return 0, fmt.Errorf("github: discover installation: status %d", resp.StatusCode)
	}
	var decoded struct {
		ID int64 `json:"id"`
	}
	if err := decodeBounded(resp, &decoded, "github: decode installation response"); err != nil {
		return 0, err
	}
	if decoded.ID == 0 {
		return 0, errors.New("github: installation response missing id")
	}

	c.mu.Lock()
	c.installations[repo] = decoded.ID
	c.mu.Unlock()
	return decoded.ID, nil
}

func decodeBounded(resp *http.Response, value any, what string) error {
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return fmt.Errorf("%s: %w", what, err)
	}
	if err := json.Unmarshal(raw, value); err != nil {
		return fmt.Errorf("%s: %w", what, err)
	}
	return nil
}

func (c *InstallationCredential) appJWT() (string, error) {
	now := time.Now()
	header := jwtHeader{Alg: "RS256", Typ: "JWT"}
	claims := jwtClaims{
		IssuedAt:  now.Add(-60 * time.Second).Unix(),
		ExpiresAt: now.Add(540 * time.Second).Unix(),
		Issuer:    fmt.Sprintf("%d", c.appID),
	}
	headerJSON, err := json.Marshal(header)
	if err != nil {
		return "", err
	}
	claimsJSON, err := json.Marshal(claims)
	if err != nil {
		return "", err
	}
	encodedHeader := base64.RawURLEncoding.EncodeToString(headerJSON)
	encodedClaims := base64.RawURLEncoding.EncodeToString(claimsJSON)
	signingInput := encodedHeader + "." + encodedClaims
	digest := sha256.Sum256([]byte(signingInput))
	signature, err := rsa.SignPKCS1v15(rand.Reader, c.key, crypto.SHA256, digest[:])
	if err != nil {
		return "", fmt.Errorf("github: sign jwt: %w", err)
	}
	return signingInput + "." + base64.RawURLEncoding.EncodeToString(signature), nil
}

func (c *InstallationCredential) RegisterSecrets(mask func(string) (string, error)) error {
	if mask == nil {
		return nil
	}
	_, err := mask(string(c.pemBytes))
	return err
}

func (c *InstallationCredential) setBaseURLForTest(baseURL string) {
	c.baseURL = strings.TrimRight(baseURL, "/")
}

func (c *InstallationCredential) Invalidate(ctx context.Context, request CredentialRequest) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	delete(c.tokens, request.Repository)
	delete(c.installations, request.Repository)
	return nil
}
