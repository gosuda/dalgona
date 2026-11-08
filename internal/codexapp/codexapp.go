package codexapp

import (
	"bytes"
	"cmp"
	"encoding/base64"
	"encoding/json/v2"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
)

type Env struct {
	Home           string
	HomeDir        string
	DalgonaHome    string
	GOOS           string
	APPDATA        string
	DesktopFile    string
	VSCodeSettings string
	CommandRunner  any
}

type Mechanism struct {
	Name      string
	Path      string
	Value     string
	Current   string
	Expected  string
	Installed bool
	Matches   bool
}

type fileMechanism struct {
	name     string
	path     func(Env) string
	enabled  func(Env) bool
	expected func(string) string
	content  func(Env, string) ([]byte, error)
}

type environmentMechanism struct {
	name    string
	current func(Env) (string, bool, error)
	set     func(Env, string) error
	restore func(Env, string, bool) error
}

type platformMechanisms struct {
	files       []fileMechanism
	environment *environmentMechanism
}

type storedFile struct {
	Path    string `json:"path"`
	Existed bool   `json:"existed"`
	Data    string `json:"data"`
}

type installRecord struct {
	Files  []storedFile `json:"files"`
	EnvSet bool         `json:"env_set"`
	Env    string       `json:"env"`
}

type pendingFile struct {
	path string
	data []byte
}

var platformMechanismsForOS = map[string]platformMechanisms{
	"linux": {
		files: []fileMechanism{linuxEnvironmentFile, vscodeFile, linuxDesktopFile},
	},
	"darwin": {
		files:       []fileMechanism{launchAgentFile, vscodeFile},
		environment: &launchctlEnvironment,
	},
	"windows": {
		files:       []fileMechanism{vscodeFile},
		environment: &windowsEnvironment,
	},
}

var errRecordNotFound = errors.New("codexapp: install record not found")

func Install(env Env, exe string) error {
	if exe == "" {
		return errors.New("codexapp: executable path is empty")
	}
	if !filepath.IsAbs(exe) {
		return errors.New("codexapp: executable path must be absolute")
	}
	home := userHome(env)
	dalgonaHome := dataHome(env, home)
	if err := os.MkdirAll(dalgonaHome, 0o700); err != nil {
		return fmt.Errorf("codexapp: create home: %w", err)
	}
	platform := activePlatform(env)
	rec, err := prepareRecord(env, dalgonaHome, platform)
	if err != nil {
		return err
	}
	files, err := renderFiles(env, exe, platform.files)
	if err != nil {
		return err
	}
	if err := saveRecord(dalgonaHome, rec); err != nil {
		return err
	}
	if err := writeFiles(files); err != nil {
		return err
	}
	return setEnvironment(env, exe, platform.environment)
}

func Uninstall(env Env) error {
	dalgonaHome := dataHome(env, userHome(env))
	rec, err := loadRecord(dalgonaHome)
	if err != nil {
		return err
	}
	if err := restoreFiles(rec.Files); err != nil {
		return err
	}
	if err := restoreEnvironment(env, rec, activePlatform(env).environment); err != nil {
		return err
	}
	if err := os.Remove(recordPath(dalgonaHome)); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("codexapp: remove install record: %w", err)
	}
	return nil
}

func Status(env Env, exe string) []Mechanism {
	platform := activePlatform(env)
	out := make([]Mechanism, 0, len(platform.files)+1)
	for _, mechanism := range platform.files {
		out = append(out, fileStatus(env, exe, mechanism))
	}
	if platform.environment != nil {
		out = append(out, environmentStatus(env, exe, *platform.environment))
	}
	return out
}

func activePlatform(env Env) platformMechanisms {
	platform, ok := platformMechanismsForOS[targetOS(env)]
	if !ok {
		platform = platformMechanisms{files: []fileMechanism{vscodeFile}}
	}
	active := platform
	active.files = make([]fileMechanism, 0, len(platform.files))
	for _, mechanism := range platform.files {
		if mechanism.enabled == nil || mechanism.enabled(env) {
			active.files = append(active.files, mechanism)
		}
	}
	return active
}

func prepareRecord(env Env, home string, platform platformMechanisms) (installRecord, error) {
	rec, err := loadRecord(home)
	if err != nil && !errors.Is(err, errRecordNotFound) {
		return installRecord{}, err
	}
	if errors.Is(err, errRecordNotFound) {
		rec = installRecord{}
	}
	if len(rec.Files) == 0 {
		files, captureErr := captureFiles(env, platform.files)
		if captureErr != nil {
			return installRecord{}, captureErr
		}
		rec.Files = files
	}
	if rec.EnvSet || platform.environment == nil {
		return rec, nil
	}
	rec.Env, rec.EnvSet, err = platform.environment.current(env)
	if err != nil {
		return installRecord{}, err
	}
	return rec, nil
}

func captureFiles(env Env, mechanisms []fileMechanism) ([]storedFile, error) {
	files := make([]storedFile, 0, len(mechanisms))
	for _, mechanism := range mechanisms {
		path := mechanism.path(env)
		file, err := captureFile(path)
		if err != nil {
			return nil, err
		}
		files = append(files, file)
	}
	return files, nil
}

func renderFiles(env Env, exe string, mechanisms []fileMechanism) ([]pendingFile, error) {
	files := make([]pendingFile, 0, len(mechanisms))
	for _, mechanism := range mechanisms {
		data, err := mechanism.content(env, exe)
		if err != nil {
			return nil, err
		}
		files = append(files, pendingFile{path: mechanism.path(env), data: data})
	}
	return files, nil
}

func writeFiles(files []pendingFile) error {
	for _, file := range files {
		if err := os.MkdirAll(filepath.Dir(file.path), 0o700); err != nil {
			return fmt.Errorf("codexapp: create parent for %s: %w", file.path, err)
		}
		if err := os.WriteFile(file.path, file.data, 0o600); err != nil {
			return fmt.Errorf("codexapp: write %s: %w", file.path, err)
		}
	}
	return nil
}

func restoreFiles(files []storedFile) error {
	for _, file := range files {
		if err := restoreFile(file); err != nil {
			return err
		}
	}
	return nil
}

func setEnvironment(env Env, exe string, mechanism *environmentMechanism) error {
	if mechanism == nil {
		return nil
	}
	if err := mechanism.set(env, exe); err != nil {
		return fmt.Errorf("codexapp: set %s environment: %w", mechanism.name, err)
	}
	return nil
}

func restoreEnvironment(env Env, rec installRecord, mechanism *environmentMechanism) error {
	if mechanism == nil {
		return nil
	}
	if err := mechanism.restore(env, rec.Env, rec.EnvSet); err != nil {
		return fmt.Errorf("codexapp: restore %s environment: %w", mechanism.name, err)
	}
	return nil
}

func fileStatus(env Env, exe string, mechanism fileMechanism) Mechanism {
	path := mechanism.path(env)
	expected := mechanism.expected(exe)
	result := Mechanism{Name: mechanism.name, Path: path, Expected: expected}
	data, err := os.ReadFile(path)
	if err != nil {
		return result
	}
	result.Value = string(data)
	result.Current = result.Value
	result.Installed = bytes.Contains(data, []byte(expected))
	result.Matches = result.Installed
	return result
}

func environmentStatus(env Env, exe string, mechanism environmentMechanism) Mechanism {
	result := Mechanism{Name: mechanism.name, Expected: exe}
	value, found, err := mechanism.current(env)
	if err != nil {
		return result
	}
	result.Value = value
	result.Current = value
	result.Installed = found && value == exe
	result.Matches = result.Installed
	return result
}

func userHome(env Env) string {
	home := cmp.Or(env.HomeDir, env.Home)
	if home != "" {
		return home
	}
	home, err := os.UserHomeDir()
	if err == nil {
		return home
	}
	return "."
}

func dataHome(env Env, home string) string {
	return cmp.Or(env.DalgonaHome, filepath.Join(home, ".dalgona"))
}

func targetOS(env Env) string {
	return cmp.Or(env.GOOS, runtime.GOOS)
}

func captureFile(path string) (storedFile, error) {
	data, err := os.ReadFile(path)
	if err == nil {
		return storedFile{Path: path, Existed: true, Data: base64.StdEncoding.EncodeToString(data)}, nil
	}
	if errors.Is(err, os.ErrNotExist) {
		return storedFile{Path: path}, nil
	}
	return storedFile{}, fmt.Errorf("codexapp: read %s: %w", path, err)
}

func restoreFile(file storedFile) error {
	if !file.Existed {
		if err := os.Remove(file.Path); err != nil && !errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("codexapp: remove %s: %w", file.Path, err)
		}
		return nil
	}
	data, err := base64.StdEncoding.DecodeString(file.Data)
	if err != nil {
		return fmt.Errorf("codexapp: decode saved file %s: %w", file.Path, err)
	}
	if err := os.MkdirAll(filepath.Dir(file.Path), 0o700); err != nil {
		return fmt.Errorf("codexapp: create parent for %s: %w", file.Path, err)
	}
	if err := os.WriteFile(file.Path, data, 0o600); err != nil {
		return fmt.Errorf("codexapp: restore %s: %w", file.Path, err)
	}
	return nil
}

func saveRecord(home string, rec installRecord) error {
	data, err := json.Marshal(rec)
	if err != nil {
		return fmt.Errorf("codexapp: encode install record: %w", err)
	}
	tmp := recordPath(home) + ".tmp"
	if err := os.WriteFile(tmp, data, 0o600); err != nil {
		return fmt.Errorf("codexapp: write install record: %w", err)
	}
	if err := os.Rename(tmp, recordPath(home)); err != nil {
		return fmt.Errorf("codexapp: publish install record: %w", err)
	}
	return nil
}

func loadRecord(home string) (installRecord, error) {
	data, err := os.ReadFile(recordPath(home))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return installRecord{}, errRecordNotFound
		}
		return installRecord{}, fmt.Errorf("codexapp: read install record: %w", err)
	}
	var rec installRecord
	if err := json.Unmarshal(data, &rec); err != nil {
		return installRecord{}, fmt.Errorf("codexapp: decode install record: %w", err)
	}
	return rec, nil
}

func recordPath(home string) string { return filepath.Join(home, "codex-install.json") }
