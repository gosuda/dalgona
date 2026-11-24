package daltoki

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"

	"github.com/gosuda/dalgona/internal/dalgona/github"
)

const (
	gitIdentityName  = "daltoki"
	gitIdentityEmail = "daltoki@users.noreply.github.com"
	deletedDigest    = "deleted"
)

var errWorktreesClosed = errors.New("daltoki: worktree manager is closed")

type WorktreeRequest struct {
	JobID      string
	Repository github.Repository
	Base       string
	Branch     string
}

type Worktree struct {
	Dir    string
	Base   string
	Branch string
}

type PushIntent struct {
	Repository github.Repository
	Branch     string
	Commit     string
}

type WorktreeManager interface {
	Prepare(ctx context.Context, request WorktreeRequest) (Worktree, error)
	Changes(ctx context.Context, dir string) (map[string]string, error)
	Commit(ctx context.Context, dir, message string) (string, error)
	Push(ctx context.Context, intent PushIntent) error
	Remove(ctx context.Context, repo github.Repository, jobID string) error
	Close() error
}

type Worktrees struct {
	root       string
	remoteBase string
	credential github.Credential
	mu         sync.Mutex
	closed     bool
	repoLocks  map[github.Repository]*sync.Mutex
	bases      map[string]string
}

func NewWorktrees(home, remoteBase string, credential github.Credential) *Worktrees {
	return &Worktrees{
		root: filepath.Join(home, "daltoki-work"), remoteBase: remoteBase, credential: credential,
		repoLocks: map[github.Repository]*sync.Mutex{}, bases: map[string]string{},
	}
}

func (w *Worktrees) mirror(repo github.Repository) string {
	return filepath.Join(w.root, "repos", repo.Owner, repo.Name+".git")
}

func (w *Worktrees) remote(repo github.Repository) string {
	return w.remoteBase + repo.Owner + "/" + repo.Name + ".git"
}

func (w *Worktrees) lock(repo github.Repository) (func(), error) {
	w.mu.Lock()
	if w.closed {
		w.mu.Unlock()
		return nil, errWorktreesClosed
	}
	repoLock, ok := w.repoLocks[repo]
	if !ok {
		repoLock = &sync.Mutex{}
		w.repoLocks[repo] = repoLock
	}
	w.mu.Unlock()
	repoLock.Lock()
	return repoLock.Unlock, nil
}

func (w *Worktrees) Close() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.closed = true
	return nil
}

func (w *Worktrees) Prepare(ctx context.Context, request WorktreeRequest) (Worktree, error) {
	if err := checkBranchNames(ctx, request.Base, request.Branch); err != nil {
		return Worktree{}, err
	}
	unlock, err := w.lock(request.Repository)
	if err != nil {
		return Worktree{}, err
	}
	defer unlock()
	mirror, err := w.fetch(ctx, request.Repository, request.Base)
	if err != nil {
		return Worktree{}, err
	}
	dir := w.jobDir(request.JobID)
	if discardErr := discardWorktree(ctx, mirror, dir); discardErr != nil {
		return Worktree{}, discardErr
	}
	baseRef := "refs/remotes/origin/" + request.Base
	args := []string{"worktree", "add", "--quiet", "--detach", dir, baseRef}
	if request.Branch != "" {
		args = []string{"worktree", "add", "--quiet", "-B", request.Branch, dir, baseRef}
	}
	if _, runErr := runGit(ctx, mirror, nil, args...); runErr != nil {
		return Worktree{}, runErr
	}
	base, err := runGit(ctx, dir, nil, "rev-parse", "HEAD")
	if err != nil {
		return Worktree{}, err
	}
	w.mu.Lock()
	w.bases[dir] = strings.TrimSpace(base)
	w.mu.Unlock()
	return Worktree{Dir: dir, Base: request.Base, Branch: request.Branch}, nil
}

func checkBranchNames(ctx context.Context, names ...string) error {
	for _, name := range names {
		if name == "" {
			continue
		}
		if strings.HasPrefix(name, "-") {
			return fmt.Errorf("daltoki: invalid branch name %q", name)
		}
		if _, err := runGit(ctx, "", nil, "check-ref-format", "refs/heads/"+name); err != nil {
			return fmt.Errorf("daltoki: invalid branch name %q", name)
		}
	}
	return nil
}

func (w *Worktrees) fetch(ctx context.Context, repo github.Repository, base string) (string, error) {
	mirror := w.mirror(repo)
	if _, err := os.Stat(filepath.Join(mirror, "HEAD")); errors.Is(err, fs.ErrNotExist) {
		if err := os.MkdirAll(mirror, 0o700); err != nil {
			return "", fmt.Errorf("daltoki: create repository mirror: %w", err)
		}
		if _, err := runGit(ctx, mirror, nil, "init", "--quiet", "--bare"); err != nil {
			return "", err
		}
	}
	auth, err := w.authorization(ctx, repo)
	if err != nil {
		return "", err
	}
	refspec := "+refs/heads/" + base + ":refs/remotes/origin/" + base
	if _, err := runGit(ctx, mirror, auth, "fetch", "--quiet", "--no-tags", w.remote(repo), refspec); err != nil {
		return "", err
	}
	return mirror, nil
}

func discardWorktree(ctx context.Context, mirror, dir string) error {
	if _, err := os.Stat(dir); errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	_, removeErr := runGit(ctx, mirror, nil, "worktree", "remove", "--force", dir)
	if err := os.RemoveAll(dir); err != nil {
		return errors.Join(removeErr, fmt.Errorf("daltoki: remove stale worktree: %w", err))
	}
	_, err := runGit(ctx, mirror, nil, "worktree", "prune")
	return err
}

func (w *Worktrees) jobDir(jobID string) string {
	return filepath.Join(w.root, "jobs", jobID)
}

func (w *Worktrees) Remove(ctx context.Context, repo github.Repository, jobID string) error {
	unlock, err := w.lock(repo)
	if err != nil {
		return err
	}
	defer unlock()
	dir := w.jobDir(jobID)
	if err := discardWorktree(ctx, w.mirror(repo), dir); err != nil {
		return err
	}
	w.mu.Lock()
	delete(w.bases, dir)
	w.mu.Unlock()
	return nil
}

func (w *Worktrees) Changes(ctx context.Context, dir string) (map[string]string, error) {
	out, err := runGit(ctx, dir, nil, "status", "--porcelain=v1", "-z", "--untracked-files=all", "--no-renames")
	if err != nil {
		return nil, err
	}
	changes := map[string]string{}
	for _, entry := range strings.Split(out, "\x00") {
		if len(entry) < 4 {
			continue
		}
		path := entry[3:]
		digest, digestErr := fileDigest(filepath.Join(dir, filepath.FromSlash(path)))
		if digestErr != nil {
			return nil, digestErr
		}
		changes[path] = digest
	}
	committed, err := w.committedSinceBase(ctx, dir)
	if err != nil {
		return nil, err
	}
	for _, path := range committed {
		if path == "" {
			continue
		}
		digest, digestErr := fileDigest(filepath.Join(dir, filepath.FromSlash(path)))
		if digestErr != nil {
			return nil, digestErr
		}
		changes[path] = digest
	}
	return changes, nil
}

func (w *Worktrees) committedSinceBase(ctx context.Context, dir string) ([]string, error) {
	w.mu.Lock()
	base, known := w.bases[dir]
	w.mu.Unlock()
	if !known || base == "" {
		return nil, nil
	}
	out, err := runGit(ctx, dir, nil, "diff", "--no-renames", "--name-only", "-z", base, "HEAD")
	if err != nil {
		return nil, err
	}
	return strings.Split(out, "\x00"), nil
}

func fileDigest(path string) (string, error) {
	info, err := os.Lstat(path)
	if errors.Is(err, fs.ErrNotExist) {
		return deletedDigest, nil
	}
	if err != nil {
		return "", fmt.Errorf("daltoki: inspect changed file: %w", err)
	}
	if info.Mode()&fs.ModeSymlink != 0 {
		target, linkErr := os.Readlink(path)
		return "link:" + target, linkErr
	}
	file, err := os.Open(path)
	if err != nil {
		return "", fmt.Errorf("daltoki: read changed file: %w", err)
	}
	defer func() { _ = file.Close() }()
	sum := sha256.New()
	if _, err := io.Copy(sum, file); err != nil {
		return "", fmt.Errorf("daltoki: read changed file: %w", err)
	}
	return hex.EncodeToString(sum.Sum(nil)), nil
}

func (w *Worktrees) Commit(ctx context.Context, dir, message string) (string, error) {
	if _, err := runGit(ctx, dir, nil, "add", "--all"); err != nil {
		return "", err
	}
	if _, err := runGit(ctx, dir, nil, "commit", "--quiet", "--no-verify", "--message", message); err != nil {
		return "", err
	}
	out, err := runGit(ctx, dir, nil, "rev-parse", "HEAD")
	return strings.TrimSpace(out), err
}

func validCommit(commit string) bool {
	if len(commit) != 40 && len(commit) != 64 {
		return false
	}
	_, err := hex.DecodeString(commit)
	return err == nil
}

func (w *Worktrees) Push(ctx context.Context, intent PushIntent) error {
	if !validCommit(intent.Commit) {
		return fmt.Errorf("daltoki: invalid push commit %q", intent.Commit)
	}
	if err := checkBranchNames(ctx, intent.Branch); err != nil {
		return err
	}
	unlock, err := w.lock(intent.Repository)
	if err != nil {
		return err
	}
	defer unlock()
	auth, err := w.authorization(ctx, intent.Repository)
	if err != nil {
		return err
	}
	refspec := intent.Commit + ":refs/heads/" + intent.Branch
	_, err = runGit(ctx, w.mirror(intent.Repository), auth, "push", "--quiet", w.remote(intent.Repository), refspec)
	return err
}

func (w *Worktrees) authorization(ctx context.Context, repo github.Repository) ([][2]string, error) {
	if w.credential == nil {
		return nil, nil
	}
	token, err := w.credential.Token(ctx, github.CredentialRequest{Repository: repo})
	if err != nil {
		return nil, fmt.Errorf("daltoki: repository credential: %w", err)
	}
	basic := base64.StdEncoding.EncodeToString([]byte("x-access-token:" + token.Value))
	return [][2]string{{"http." + w.remoteBase + ".extraHeader", "Authorization: Basic " + basic}}, nil
}

func gitEnvironment(extra [][2]string) []string {
	env := make([]string, 0, len(os.Environ())+16)
	for _, entry := range os.Environ() {
		if !strings.HasPrefix(entry, "GIT_") {
			env = append(env, entry)
		}
	}
	env = append(env,
		"GIT_TERMINAL_PROMPT=0", "GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_GLOBAL="+os.DevNull,
		"GIT_AUTHOR_NAME="+gitIdentityName, "GIT_AUTHOR_EMAIL="+gitIdentityEmail,
		"GIT_COMMITTER_NAME="+gitIdentityName, "GIT_COMMITTER_EMAIL="+gitIdentityEmail,
	)
	settings := append([][2]string{{"credential.helper", ""}, {"core.hooksPath", os.DevNull}, {"commit.gpgSign", "false"}}, extra...)
	env = append(env, "GIT_CONFIG_COUNT="+strconv.Itoa(len(settings)))
	for i, setting := range settings {
		index := strconv.Itoa(i)
		env = append(env, "GIT_CONFIG_KEY_"+index+"="+setting[0], "GIT_CONFIG_VALUE_"+index+"="+setting[1])
	}
	return env
}

func runGit(ctx context.Context, dir string, extra [][2]string, args ...string) (string, error) {
	full := args
	if dir != "" {
		full = append([]string{"-C", dir}, args...)
	}
	cmd := exec.CommandContext(ctx, "git", full...)
	cmd.Env = gitEnvironment(extra)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		return "", fmt.Errorf("daltoki: git %s: %w: %s", args[0], err, strings.TrimSpace(stderr.String()))
	}
	return stdout.String(), nil
}
