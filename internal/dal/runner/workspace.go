package runner

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"time"
	"unicode/utf8"
)

const (
	WorkspaceListLimit     = 10000
	WorkspaceChunkLimit    = 1 << 20
	WorkspaceAbsent        = "absent"
	workspaceStagingPrefix = ".dalgona-upload-"
	workspaceFileMode      = 0o644
	workspaceDirMode       = 0o755
	workspaceStagingMode   = 0o600
	syncsDirectories       = runtime.GOOS != "windows"
)

type WorkspaceOp string

const (
	WorkspaceStat    WorkspaceOp = "stat"
	WorkspaceList    WorkspaceOp = "list"
	WorkspaceRead    WorkspaceOp = "read"
	WorkspaceStage   WorkspaceOp = "stage"
	WorkspacePublish WorkspaceOp = "publish"
	WorkspaceDiscard WorkspaceOp = "discard"
	WorkspaceMkdir   WorkspaceOp = "mkdir"
	WorkspaceRename  WorkspaceOp = "rename"
	WorkspaceRemove  WorkspaceOp = "remove"
)

func (op WorkspaceOp) Reads() bool {
	return op == WorkspaceStat || op == WorkspaceList || op == WorkspaceRead
}

type WorkspaceRequest struct {
	Op        WorkspaceOp `json:"op"`
	Root      string      `json:"root"`
	Token     string      `json:"token,omitzero"`
	Path      string      `json:"path,omitzero"`
	To        string      `json:"to,omitzero"`
	Staging   string      `json:"staging,omitzero"`
	Expected  string      `json:"expectedRevision,omitzero"`
	Offset    int64       `json:"offset,omitzero"`
	Limit     int64       `json:"limit,omitzero"`
	Data      []byte      `json:"dataBase64,omitzero"`
	Recursive bool        `json:"recursive,omitzero"`
	Replace   bool        `json:"replace,omitzero"`
}

type WorkspaceEntry struct {
	Name     string    `json:"name"`
	Kind     string    `json:"kind"`
	Size     int64     `json:"size"`
	ModTime  time.Time `json:"modTime"`
	Revision string    `json:"revision"`
	Readable bool      `json:"readable"`
	Writable bool      `json:"writable"`
}

type WorkspaceResult struct {
	Root     string           `json:"root"`
	Entry    WorkspaceEntry   `json:"entry,omitzero"`
	Entries  []WorkspaceEntry `json:"entries,omitzero"`
	Revision string           `json:"revision,omitzero"`
	Total    int64            `json:"total,omitzero"`
	Data     []byte           `json:"dataBase64,omitzero"`
	Staging  string           `json:"staging,omitzero"`
}

type Workspace interface {
	Workspace(ctx context.Context, req WorkspaceRequest) (WorkspaceResult, error)
}

type WorkspaceHandle interface {
	Workspace(ctx context.Context, req WorkspaceRequest) (WorkspaceResult, error)
	DisplayRoot() string
	VerifyExec(ctx context.Context) error
	Close() error
}

type WorkspaceOpener interface {
	OpenWorkspace(ctx context.Context, cwd string) (WorkspaceHandle, error)
}

var (
	ErrWorkspaceEscape   = errors.New("runner: path leaves the workspace root")
	ErrWorkspaceKind     = errors.New("runner: path has the wrong file kind for this operation")
	ErrWorkspaceExists   = errors.New("runner: destination already exists")
	ErrWorkspaceRoot     = errors.New("runner: operation refuses the workspace root")
	ErrWorkspaceLimit    = errors.New("runner: directory holds more entries than the workspace list limit")
	ErrWorkspaceOffset   = errors.New("runner: staged upload is not at the requested offset")
	ErrWorkspaceNotEmpty = errors.New("runner: directory is not empty")
	ErrWorkspaceRequest  = errors.New("runner: malformed workspace request")
)

var ErrWorkspaceChanged = errors.New("runner: workspace root changed")

var ErrWorkspaceUnsupported = errors.New("runner: workspace pinning not supported")

type StaleRevisionError struct{ Current string }

func (e *StaleRevisionError) Error() string {
	return "runner: revision changed; current revision is " + e.Current
}

var _ Workspace = (*Local)(nil)

func (l *Local) Workspace(ctx context.Context, req WorkspaceRequest) (WorkspaceResult, error) {
	if err := ctx.Err(); err != nil {
		return WorkspaceResult{}, fmt.Errorf("runner: workspace: %w", err)
	}
	op, ok := workspaceOps[req.Op]
	if !ok {
		return WorkspaceResult{}, ErrWorkspaceRequest
	}
	root, err := os.OpenRoot(req.Root)
	if err != nil {
		return WorkspaceResult{}, fmt.Errorf("runner: open workspace root: %w", err)
	}
	defer func() { _ = root.Close() }()
	result, err := op(root, req)
	result.Root = root.Name()
	return result, workspaceError(err)
}

var workspaceOps = map[WorkspaceOp]func(*os.Root, WorkspaceRequest) (WorkspaceResult, error){
	WorkspaceStat:    workspaceStat,
	WorkspaceList:    workspaceList,
	WorkspaceRead:    workspaceRead,
	WorkspaceStage:   workspaceStage,
	WorkspacePublish: workspacePublish,
	WorkspaceDiscard: workspaceDiscard,
	WorkspaceMkdir:   workspaceMkdir,
	WorkspaceRename:  workspaceRename,
	WorkspaceRemove:  workspaceRemove,
}

func workspaceError(err error) error {
	pathErr, ok := errors.AsType[*fs.PathError](err)
	if ok && pathErr.Err.Error() == "path escapes from parent" {
		return fmt.Errorf("%w: %s", ErrWorkspaceEscape, pathErr.Path)
	}
	if errors.Is(err, fs.ErrExist) && !errors.Is(err, ErrWorkspaceExists) {
		return fmt.Errorf("%w: %w", ErrWorkspaceExists, err)
	}
	return err
}

func localName(rel string) string {
	if rel == "" {
		return "."
	}
	return filepath.FromSlash(rel)
}

func isRootPath(rel string) bool { return rel == "" || rel == "." }

func workspaceStat(root *os.Root, req WorkspaceRequest) (WorkspaceResult, error) {
	entry, err := lstatEntry(root, req.Path)
	return WorkspaceResult{Entry: entry}, err
}

func lstatEntry(root *os.Root, rel string) (WorkspaceEntry, error) {
	info, err := root.Lstat(localName(rel))
	if err != nil {
		return WorkspaceEntry{}, err
	}
	return entryOf(path.Base(rel), info, rootContent(root, rel)), nil
}

func rootContent(root *os.Root, rel string) func() (io.ReadCloser, error) {
	return func() (io.ReadCloser, error) { return root.OpenFile(localName(rel), os.O_RDONLY|workspaceReadFlag, 0) }
}

func entryOf(name string, info fs.FileInfo, content func() (io.ReadCloser, error)) WorkspaceEntry {
	if name == "." {
		name = ""
	}
	perm := info.Mode().Perm()
	return WorkspaceEntry{
		Name: name, Kind: entryKind(info.Mode()), Size: info.Size(), ModTime: info.ModTime().UTC(),
		Revision: revisionOf(info, content), Readable: perm&0o400 != 0, Writable: perm&0o200 != 0,
	}
}

func entryKind(mode fs.FileMode) string {
	switch {
	case mode.IsRegular():
		return "file"
	case mode.IsDir():
		return "directory"
	case mode&fs.ModeSymlink != 0:
		return "symlink"
	}
	return "other"
}

func revisionOf(info fs.FileInfo, content func() (io.ReadCloser, error)) string {
	digest := sha256.New()
	var fixed [32]byte
	binary.BigEndian.PutUint64(fixed[0:], uint64(info.Mode()))
	binary.BigEndian.PutUint64(fixed[8:], uint64(info.Size()))                //nolint:gosec // the size bits feed a digest
	binary.BigEndian.PutUint64(fixed[16:], uint64(info.ModTime().UnixNano())) //nolint:gosec // the time bits feed a digest
	digest.Write(fixed[:24])
	digest.Write(fileIdentity(info, content))
	return hex.EncodeToString(digest.Sum(nil))
}

func fileIdentity(info fs.FileInfo, content func() (io.ReadCloser, error)) []byte {
	if !info.Mode().IsRegular() {
		return nonFileIdentity(info)
	}
	reader, err := content()
	if err != nil {
		return nil
	}
	defer func() { _ = reader.Close() }()
	digest := sha256.New()
	if _, err := io.Copy(digest, reader); err != nil {
		return nil
	}
	return digest.Sum(nil)
}

func workspaceList(root *os.Root, req WorkspaceRequest) (WorkspaceResult, error) {
	dir, err := openWorkspaceDir(root, req.Path)
	if err != nil {
		return WorkspaceResult{}, err
	}
	defer func() { _ = dir.Close() }()
	names, err := dir.Readdirnames(WorkspaceListLimit + 1)
	if err != nil && !errors.Is(err, io.EOF) {
		return WorkspaceResult{}, err
	}
	if len(names) > WorkspaceListLimit {
		return WorkspaceResult{}, ErrWorkspaceLimit
	}
	slices.Sort(names)
	entries := make([]WorkspaceEntry, 0, len(names))
	for _, name := range names {
		entry, skip, err := listEntry(root, req.Path, name)
		if err != nil {
			return WorkspaceResult{}, err
		}
		if skip {
			continue
		}
		entries = append(entries, entry)
	}
	return WorkspaceResult{Entries: entries, Revision: ListingRevision(entries)}, nil
}

func openWorkspaceDir(root *os.Root, rel string) (*os.File, error) {
	info, err := root.Lstat(localName(rel))
	if err != nil {
		return nil, err
	}
	if info.Mode()&fs.ModeSymlink == 0 && !info.IsDir() {
		return nil, ErrWorkspaceKind
	}
	dir, err := root.OpenFile(localName(rel), os.O_RDONLY|workspaceReadFlag, 0)
	if err != nil {
		return nil, err
	}
	info, err = dir.Stat()
	if err != nil {
		_ = dir.Close()
		return nil, err
	}
	if !info.IsDir() {
		_ = dir.Close()
		return nil, ErrWorkspaceKind
	}
	return dir, nil
}

func listEntry(root *os.Root, base, name string) (WorkspaceEntry, bool, error) {
	if !utf8.ValidString(name) {
		return WorkspaceEntry{}, true, nil
	}
	entry, err := lstatEntry(root, path.Join(base, name))
	if errors.Is(err, fs.ErrNotExist) {
		return WorkspaceEntry{}, true, nil
	}
	if err != nil {
		return WorkspaceEntry{}, false, err
	}
	return entry, false, nil
}

func ListingRevision(entries []WorkspaceEntry) string {
	digest := sha256.New()
	for _, entry := range entries {
		digest.Write([]byte(entry.Name))
		digest.Write([]byte{0})
		digest.Write([]byte(entry.Revision))
		digest.Write([]byte{0})
	}
	return hex.EncodeToString(digest.Sum(nil))
}

func workspaceRead(root *os.Root, req WorkspaceRequest) (WorkspaceResult, error) {
	if req.Offset < 0 || req.Limit < 0 {
		return WorkspaceResult{}, ErrWorkspaceRequest
	}
	file, err := root.OpenFile(localName(req.Path), os.O_RDONLY|workspaceReadFlag, 0)
	if err != nil {
		return WorkspaceResult{}, err
	}
	defer func() { _ = file.Close() }()
	revision, size, err := openRevision(file)
	if err != nil {
		return WorkspaceResult{}, err
	}
	if matchErr := matchRevision(req.Expected, revision); matchErr != nil {
		return WorkspaceResult{}, matchErr
	}
	limit := min(chunkLimit(req.Limit), max(size-req.Offset, 0))
	data := make([]byte, limit)
	n, err := file.ReadAt(data, req.Offset)
	if err != nil && !errors.Is(err, io.EOF) {
		return WorkspaceResult{}, err
	}
	if err := rereadRevision(file, revision); err != nil {
		return WorkspaceResult{}, err
	}
	return WorkspaceResult{Revision: revision, Total: size, Data: data[:n]}, nil
}

func matchRevision(expected, revision string) error {
	if expected != "" && expected != revision {
		return &StaleRevisionError{Current: revision}
	}
	return nil
}

func rereadRevision(file *os.File, revision string) error {
	after, _, err := openRevision(file)
	if err != nil {
		return err
	}
	if after != revision {
		return &StaleRevisionError{Current: after}
	}
	return nil
}

func chunkLimit(limit int64) int64 {
	if limit <= 0 || limit > WorkspaceChunkLimit {
		return WorkspaceChunkLimit
	}
	return limit
}

func openRevision(file *os.File) (string, int64, error) {
	info, err := file.Stat()
	if err != nil {
		return "", 0, err
	}
	if !info.Mode().IsRegular() {
		return "", 0, ErrWorkspaceKind
	}
	content := func() (io.ReadCloser, error) { return io.NopCloser(io.NewSectionReader(file, 0, info.Size())), nil }
	return revisionOf(info, content), info.Size(), nil
}

func isStaging(rel string) bool {
	return strings.HasPrefix(path.Base(rel), workspaceStagingPrefix) && !isRootPath(rel)
}

func workspaceStage(root *os.Root, req WorkspaceRequest) (WorkspaceResult, error) {
	if req.Staging == "" {
		return stageFirst(root, req)
	}
	if !isStaging(req.Staging) {
		return WorkspaceResult{}, ErrWorkspaceRequest
	}
	file, err := root.OpenFile(localName(req.Staging), os.O_WRONLY|workspaceReadFlag, 0)
	if err != nil {
		return WorkspaceResult{}, err
	}
	defer func() { _ = file.Close() }()
	info, err := file.Stat()
	if err != nil {
		return WorkspaceResult{}, err
	}
	if !info.Mode().IsRegular() || info.Size() != req.Offset {
		return WorkspaceResult{}, ErrWorkspaceOffset
	}
	if _, err := file.WriteAt(req.Data, req.Offset); err != nil {
		return WorkspaceResult{}, err
	}
	return WorkspaceResult{Staging: req.Staging}, file.Sync()
}

func stageFirst(root *os.Root, req WorkspaceRequest) (WorkspaceResult, error) {
	if req.Offset != 0 || isRootPath(req.Path) {
		return WorkspaceResult{}, ErrWorkspaceRequest
	}
	var random [12]byte
	if _, err := rand.Read(random[:]); err != nil {
		return WorkspaceResult{}, fmt.Errorf("runner: staging name: %w", err)
	}
	staging := path.Join(path.Dir(req.Path), workspaceStagingPrefix+hex.EncodeToString(random[:]))
	file, err := root.OpenFile(localName(staging), os.O_WRONLY|os.O_CREATE|os.O_EXCL, workspaceStagingMode)
	if err != nil {
		return WorkspaceResult{}, err
	}
	_, writeErr := file.Write(req.Data)
	syncErr := file.Sync()
	if err := errors.Join(writeErr, syncErr, file.Close()); err != nil {
		_ = root.Remove(localName(staging))
		return WorkspaceResult{}, err
	}
	return WorkspaceResult{Staging: staging}, nil
}

func workspacePublish(root *os.Root, req WorkspaceRequest) (result WorkspaceResult, err error) {
	if !isStaging(req.Staging) || isRootPath(req.Path) || req.Expected == "" {
		return WorkspaceResult{}, ErrWorkspaceRequest
	}
	defer func() {
		if err != nil {
			_ = root.Remove(localName(req.Staging))
		}
	}()
	current, mode, err := publishTarget(root, req.Path)
	if err != nil {
		return WorkspaceResult{}, err
	}
	if current != req.Expected {
		return WorkspaceResult{}, &StaleRevisionError{Current: current}
	}
	if publishErr := applyPublish(root, req, mode); publishErr != nil {
		return WorkspaceResult{}, publishErr
	}
	entry, err := lstatEntry(root, req.Path)
	return WorkspaceResult{Entry: entry}, err
}

func applyPublish(root *os.Root, req WorkspaceRequest, mode fs.FileMode) error {
	if req.Expected == WorkspaceAbsent {
		return publishAbsent(root, req, mode)
	}
	return publishStaged(root, req, mode)
}

func publishAbsent(root *os.Root, req WorkspaceRequest, mode fs.FileMode) error {
	if err := root.Chmod(localName(req.Staging), mode); err != nil {
		return err
	}
	if err := root.Link(localName(req.Staging), localName(req.Path)); err != nil {
		return absentConflict(root, req, err)
	}
	if err := root.Remove(localName(req.Staging)); err != nil {
		_ = root.Remove(localName(req.Path))
		return err
	}
	return syncRootDir(root, path.Dir(req.Path))
}

func absentConflict(root *os.Root, req WorkspaceRequest, linkErr error) error {
	current, _, err := publishTarget(root, req.Path)
	if err != nil {
		return err
	}
	if current == WorkspaceAbsent {
		return linkErr
	}
	return &StaleRevisionError{Current: current}
}

func publishStaged(root *os.Root, req WorkspaceRequest, mode fs.FileMode) error {
	if err := root.Chmod(localName(req.Staging), mode); err != nil {
		return err
	}
	if err := root.Rename(localName(req.Staging), localName(req.Path)); err != nil {
		return err
	}
	return syncRootDir(root, path.Dir(req.Path))
}

func publishTarget(root *os.Root, rel string) (string, fs.FileMode, error) {
	info, err := root.Lstat(localName(rel))
	if errors.Is(err, fs.ErrNotExist) {
		return WorkspaceAbsent, workspaceFileMode, nil
	}
	if err != nil {
		return "", 0, err
	}
	if !info.Mode().IsRegular() {
		return "", 0, ErrWorkspaceKind
	}
	return revisionOf(info, rootContent(root, rel)), info.Mode().Perm(), nil
}

func syncRootDir(root *os.Root, rel string) error {
	if !syncsDirectories {
		return nil
	}
	dir, err := root.Open(localName(rel))
	if err != nil {
		return err
	}
	return errors.Join(dir.Sync(), dir.Close())
}

func workspaceDiscard(root *os.Root, req WorkspaceRequest) (WorkspaceResult, error) {
	if !isStaging(req.Staging) {
		return WorkspaceResult{}, ErrWorkspaceRequest
	}
	err := root.Remove(localName(req.Staging))
	if errors.Is(err, fs.ErrNotExist) {
		err = nil
	}
	return WorkspaceResult{}, err
}

func workspaceMkdir(root *os.Root, req WorkspaceRequest) (WorkspaceResult, error) {
	if isRootPath(req.Path) {
		return WorkspaceResult{}, ErrWorkspaceRoot
	}
	mkdir := root.Mkdir
	if req.Recursive {
		mkdir = root.MkdirAll
	}
	if err := mkdir(localName(req.Path), workspaceDirMode); err != nil {
		return WorkspaceResult{}, err
	}
	if err := syncRootDir(root, path.Dir(req.Path)); err != nil {
		return WorkspaceResult{}, err
	}
	entry, err := lstatEntry(root, req.Path)
	if err == nil && entry.Kind != "directory" {
		err = ErrWorkspaceKind
	}
	return WorkspaceResult{Entry: entry}, err
}

func workspaceRename(root *os.Root, req WorkspaceRequest) (WorkspaceResult, error) {
	if isRootPath(req.Path) || isRootPath(req.To) {
		return WorkspaceResult{}, ErrWorkspaceRoot
	}
	if err := expectRevision(root, req.Path, req.Expected); err != nil {
		return WorkspaceResult{}, err
	}
	if !req.Replace {
		if err := renameNoReplace(root, localName(req.Path), localName(req.To)); err != nil {
			return WorkspaceResult{}, err
		}
	} else if err := root.Rename(localName(req.Path), localName(req.To)); err != nil {
		return WorkspaceResult{}, err
	}
	if err := errors.Join(syncRootDir(root, path.Dir(req.Path)), syncRootDir(root, path.Dir(req.To))); err != nil {
		return WorkspaceResult{}, err
	}
	entry, err := lstatEntry(root, req.To)
	return WorkspaceResult{Entry: entry}, err
}

func renameNoReplace(root *os.Root, oldName, newName string) error {
	info, err := root.Lstat(oldName)
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() {
		return renameDirNoReplace(root, oldName, newName)
	}
	if err := root.Link(oldName, newName); err != nil {
		if errors.Is(err, fs.ErrExist) {
			return ErrWorkspaceExists
		}
		return err
	}
	if err := root.Remove(oldName); err != nil {
		_ = root.Remove(newName)
		return err
	}
	return nil
}

func expectRevision(root *os.Root, rel, expected string) error {
	entry, err := lstatEntry(root, rel)
	if err != nil {
		return err
	}
	if entry.Revision != expected {
		return &StaleRevisionError{Current: entry.Revision}
	}
	return nil
}

func workspaceRemove(root *os.Root, req WorkspaceRequest) (WorkspaceResult, error) {
	if isRootPath(req.Path) {
		return WorkspaceResult{}, ErrWorkspaceRoot
	}
	if err := expectRevision(root, req.Path, req.Expected); err != nil {
		return WorkspaceResult{}, err
	}
	remove := root.Remove
	if req.Recursive {
		remove = root.RemoveAll
	} else if err := requireEmpty(root, req.Path); err != nil {
		return WorkspaceResult{}, err
	}
	if err := remove(localName(req.Path)); err != nil {
		return WorkspaceResult{}, err
	}
	return WorkspaceResult{}, syncRootDir(root, path.Dir(req.Path))
}

func requireEmpty(root *os.Root, rel string) error {
	info, err := root.Lstat(localName(rel))
	if err != nil || !info.IsDir() {
		return err
	}
	dir, err := root.Open(localName(rel))
	if err != nil {
		return err
	}
	defer func() { _ = dir.Close() }()
	names, err := dir.Readdirnames(1)
	if len(names) > 0 {
		return ErrWorkspaceNotEmpty
	}
	if errors.Is(err, io.EOF) {
		return nil
	}
	return err
}
