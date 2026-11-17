package report

import (
	"context"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"time"

	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
)

type AppSource struct {
	access  string
	expires time.Time
	now     func() time.Time
}

func NewAppSource(home string, now func() time.Time) (*AppSource, error) {
	if now == nil {
		now = time.Now
	}
	source := &AppSource{now: now}
	credential, err := readCredential(home)
	if err != nil {
		return source, nil
	}
	source.access = credential.Token
	source.expires = credential.Expires
	return source, nil
}

func (s *AppSource) Credential(ctx context.Context) (Credential, error) {
	if err := ctx.Err(); err != nil {
		return Credential{}, err
	}
	if s.access == "" || (!s.expires.IsZero() && !s.now().Before(s.expires)) {
		return Credential{}, errors.New("failure reporting authorization expired; run dalgona failures reporting login")
	}
	return Credential{Access: s.access, Expires: s.expires}, nil
}

func ResolveReportRepository(ctx context.Context) (int64, error) {
	deadline, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(deadline, http.MethodGet, "https://api.github.com/repos/"+reportOwner+"/"+reportRepo, nil)
	if err != nil {
		return 0, err
	}
	req.Header.Set("Accept", "application/vnd.github+json")
	response, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, fmt.Errorf("failure reporting repository metadata: %w", err)
	}
	defer func() { _ = response.Body.Close() }()
	body, err := io.ReadAll(io.LimitReader(response.Body, 1<<20))
	if err != nil {
		return 0, fmt.Errorf("failure reporting repository metadata: %w", err)
	}
	if response.StatusCode != http.StatusOK {
		return 0, fmt.Errorf("failure reporting repository metadata: status %d", response.StatusCode)
	}
	var info struct {
		ID       int64  `json:"id"`
		FullName string `json:"full_name"`
	}
	if err := json.Unmarshal(body, &info); err != nil {
		return 0, errors.New("failure reporting repository metadata is invalid")
	}
	if info.ID <= 0 || info.FullName != "gosuda/dalgona" {
		return 0, errors.New("failure reporting repository metadata is invalid")
	}
	return info.ID, nil
}

func AcquireAppToken(ctx context.Context, home string, repositoryID int64, show func(string, string)) error {
	if reportAppClientID == "" {
		return errors.New("failure reporting is unavailable: github app client id is missing")
	}
	token, err := oauth.DeviceCode(ctx, oauth.Provider{ //nolint:gosec // G101: public OAuth client id endpoints, not credentials
		ID:               "failure-reporting",
		DeviceURL:        "https://github.com/login/device/code",
		TokenURL:         "https://github.com/login/oauth/access_token",
		ClientID:         reportAppClientID,
		Scopes:           []string{"offline_access"},
		DevicePollParams: map[string]string{"repository_id": strconv.FormatInt(repositoryID, 10)},
	}, show)
	if err != nil {
		return err
	}
	return writeCredential(home, token.Access, token.Expires)
}
