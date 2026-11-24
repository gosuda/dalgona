package report

import (
	"crypto/sha256"
	"encoding/hex"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"time"
)

const (
	consentFileName    = "consent.json"
	credentialFileName = "credential.json" //nolint:gosec // G101: a file name, not a credential value
)

const consentMethodGH = "gh"

type consentFile struct {
	Version    int       `json:"version"`
	At         time.Time `json:"at"`
	Method     string    `json:"method"`
	TextSHA256 string    `json:"textSha256"`
}

type storedCredential struct {
	Version int       `json:"version"`
	Token   string    `json:"token"`
	Expires time.Time `json:"expires"`
}

func ConsentText(method string) string {
	return "dalgona files failure reports to gosuda/dalgona as public GitHub issues.\n" +
		"Each report includes the product version, OS, architecture, failure class, tool name, fingerprint,\n" +
		"a masked error and stack, and a bounded masked excerpt of the failing tool arguments and output.\n" +
		"Reports never include prompts, conversation history, or absolute paths.\n" +
		"Authentication: " + method + ".\n"
}

func ConsentTextHash(method string) string {
	sum := sha256.Sum256([]byte(ConsentText(method)))
	return hex.EncodeToString(sum[:])
}

func WriteConsent(home, method string, at time.Time) error {
	dir := filepath.Join(home, reportDirName)
	if err := ensureReporterDir(dir); err != nil {
		return err
	}
	data, err := json.Marshal(consentFile{
		Version:    1,
		At:         at.UTC(),
		Method:     method,
		TextSHA256: ConsentTextHash(method),
	})
	if err != nil {
		return fmt.Errorf("failure reporting write local state: %w", err)
	}
	return atomicWriteFile(dir, consentFileName, data)
}

func ConsentValid(home, method string) bool {
	stored, err := readConsent(home)
	if err != nil {
		return false
	}
	return stored.Method == method && stored.TextSHA256 == ConsentTextHash(method)
}

func ConsentStored(home string) bool {
	_, err := readConsent(home)
	return err == nil
}

func readConsent(home string) (consentFile, error) {
	var stored consentFile
	raw, err := os.ReadFile(filepath.Join(home, reportDirName, consentFileName))
	if err != nil {
		return stored, err
	}
	if err := json.Unmarshal(raw, &stored); err != nil {
		return stored, err
	}
	if stored.Version != 1 {
		return stored, errors.New("failure reporting stopped: state is invalid")
	}
	return stored, nil
}

func writeCredential(home, token string, expires time.Time) error {
	dir := filepath.Join(home, reportDirName)
	if err := ensureReporterDir(dir); err != nil {
		return err
	}
	data, err := json.Marshal(storedCredential{Version: 1, Token: token, Expires: expires.UTC()})
	if err != nil {
		return fmt.Errorf("failure reporting write local state: %w", err)
	}
	return atomicWriteFile(dir, credentialFileName, data)
}

func readCredential(home string) (storedCredential, error) {
	var stored storedCredential
	raw, err := os.ReadFile(filepath.Join(home, reportDirName, credentialFileName))
	if err != nil {
		return stored, err
	}
	if err := json.Unmarshal(raw, &stored); err != nil {
		return stored, errors.New("failure reporting stopped: state is invalid")
	}
	if stored.Version != 1 || stored.Token == "" {
		return stored, errors.New("failure reporting stopped: state is invalid")
	}
	return stored, nil
}

func Disable(home string) error {
	dir := filepath.Join(home, reportDirName)
	var errs []error
	for _, name := range []string{consentFileName, credentialFileName, queueFileName} {
		err := os.Remove(filepath.Join(dir, name))
		if err != nil && !errors.Is(err, fs.ErrNotExist) {
			errs = append(errs, err)
		}
	}
	syncReporterParent(dir)
	return errors.Join(errs...)
}
