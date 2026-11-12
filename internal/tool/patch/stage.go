package patch

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io/fs"
	"path/filepath"
	"regexp"
	"slices"
	"strings"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type replacement struct {
	index  int
	start  int
	end    int
	body   []byte
	splice snapshot.Splice
}

type stagedFile struct {
	path      string
	absolute  string
	before    []byte
	after     []byte
	exists    bool
	deleted   bool
	mode      fs.FileMode
	renameTo  string
	renameAbs string
	pin       *dirPin
	renamePin *dirPin
	splices   []snapshot.Splice
	sound     bool
}

type stagedBatch struct {
	style Style
	files []*stagedFile
}

type fileGroup struct {
	display  string
	absolute string
	edits    []edit
}

func stage(ctx context.Context, style Style, edits []edit, env tool.Env) (stagedBatch, error) {
	groups, err := groupEdits(ctx, env, edits)
	if err != nil {
		return stagedBatch{}, err
	}
	if len(groups) > 64 {
		return stagedBatch{}, fmt.Errorf("patch: this call touches %d files; the limit is 64", len(groups))
	}
	files, err := stageGroups(ctx, style, groups, env)
	if err != nil {
		return stagedBatch{}, err
	}
	if err := checkOmissions(files); err != nil {
		closeFiles(files)
		return stagedBatch{}, err
	}
	if len(files) == 0 && !style.profile() {
		return stagedBatch{}, errors.New("patch: the edits produce no change")
	}
	if err := checkRenameTargets(files); err != nil {
		closeFiles(files)
		return stagedBatch{}, err
	}
	return stagedBatch{style: style, files: files}, nil
}

func stageGroups(ctx context.Context, style Style, groups []fileGroup, env tool.Env) ([]*stagedFile, error) {
	files := make([]*stagedFile, 0, len(groups))
	stagedBytes := 0
	for _, group := range groups {
		file, err := stageFile(ctx, style, group, env)
		if err != nil {
			closeFiles(files)
			return nil, err
		}
		if file == nil {
			continue
		}
		stagedBytes += len(file.before) + len(file.after)
		if stagedBytes > 64<<20 {
			file.closePins()
			closeFiles(files)
			return nil, errors.New("patch: this call stages over 64 MiB; split it into several calls")
		}
		if file.pin, err = pinVerifiedComponents(env, group.display, group.absolute); err != nil {
			file.closePins()
			closeFiles(files)
			return nil, err
		}
		files = append(files, file)
	}
	return files, nil
}

func groupEdits(ctx context.Context, env tool.Env, edits []edit) ([]fileGroup, error) {
	byPath := make(map[string]*fileGroup)
	for _, item := range edits {
		display, absolute, err := resolveEnvPath(ctx, env, item.path)
		if err != nil {
			return nil, err
		}
		group := byPath[display]
		if group == nil {
			group = &fileGroup{display: display, absolute: absolute}
			byPath[display] = group
		}
		group.edits = append(group.edits, item)
	}
	groups := make([]fileGroup, 0, len(byPath))
	for _, group := range byPath {
		groups = append(groups, *group)
	}
	slices.SortFunc(groups, func(a, b fileGroup) int { return strings.Compare(a.display, b.display) })
	return groups, nil
}

func resolveEnvPath(ctx context.Context, env tool.Env, raw string) (string, string, error) {
	display, absolute, err := resolvePatchPath(env.Cwd, raw)
	if err != nil {
		return "", "", err
	}
	if err := checkPathComponents(ctx, env, display, absolute); err != nil {
		return "", "", err
	}
	return display, absolute, nil
}

func checkPathComponents(ctx context.Context, env tool.Env, display, absolute string) error {
	rel, err := filepath.Rel(env.Cwd, absolute)
	if err != nil {
		return fmt.Errorf("patch: resolve %s: %w", display, err)
	}
	current := env.Cwd
	for component := range strings.SplitSeq(rel, string(filepath.Separator)) {
		entries, err := env.Runner.ReadDir(ctx, current)
		if errors.Is(err, fs.ErrNotExist) {
			return nil
		}
		if err != nil {
			return fmt.Errorf("patch: inspect path %s: %w", display, err)
		}
		info, found := namedFile(entries, component)
		if !found {
			return nil
		}
		if info.Mode&fs.ModeSymlink != 0 {
			return fmt.Errorf("patch: %s contains a symbolic link; patch does not follow links", display)
		}
		current = filepath.Join(current, component)
	}
	return nil
}

func namedFile(entries []runner.FileInfo, name string) (runner.FileInfo, bool) {
	for _, entry := range entries {
		if entry.Name == name {
			return entry, true
		}
	}
	return runner.FileInfo{}, false
}

func hasParentComponent(path string) bool {
	for part := range strings.SplitSeq(filepath.ToSlash(path), "/") {
		if part == ".." {
			return true
		}
	}
	return false
}

func resolvePatchPath(cwd, raw string) (string, string, error) {
	if raw == "" {
		return "", "", errors.New("patch: path must not be empty")
	}
	if rootedOrVolume.MatchString(raw) || strings.IndexByte(raw, 0) >= 0 {
		return "", "", fmt.Errorf("patch: %s is outside the workspace %s", raw, cwd)
	}
	clean := filepath.Clean(raw)
	if clean == "." || clean == ".." || strings.HasPrefix(clean, ".."+string(filepath.Separator)) {
		return "", "", fmt.Errorf("patch: %s is outside the workspace %s", raw, cwd)
	}
	return filepath.ToSlash(clean), filepath.Join(cwd, clean), nil
}

var rootedOrVolume = regexp.MustCompile(`^(?:[A-Za-z]:|[/\\])`)

func stageFile(ctx context.Context, style Style, group fileGroup, env tool.Env) (*stagedFile, error) {
	if len(group.edits) == 1 && group.edits[0].kind == editCreate {
		return stageCreate(ctx, group, env)
	}
	if len(group.edits) == 1 && group.edits[0].kind == editDelete {
		return stageDelete(ctx, group, env)
	}
	return stageUpdate(ctx, style, group, env)
}

func stageUpdate(ctx context.Context, style Style, group fileGroup, env tool.Env) (*stagedFile, error) {
	if err := validateChangeShapes(group); err != nil {
		return nil, err
	}
	before, info, err := readTarget(ctx, env, group.absolute, group.display)
	if err != nil {
		return nil, err
	}
	doc, err := decodeDocument(group.display, before)
	if err != nil {
		return nil, err
	}
	file := &stagedFile{
		path: group.display, absolute: group.absolute, before: before,
		exists: true, mode: info.mode, sound: true,
	}
	resolver := fileResolver{style: style, group: group, env: env, file: file}
	if err := resolver.stageContent(ctx, doc); err != nil {
		file.closePins()
		return nil, err
	}
	if bytes.Equal(before, file.after) && file.renameTo == "" {
		return nil, nil
	}
	return file, nil
}

func stageCreate(ctx context.Context, group fileGroup, env tool.Env) (*stagedFile, error) {
	item := group.edits[0]
	_, err := env.Runner.Stat(ctx, group.absolute)
	if err == nil {
		return nil, fmt.Errorf("patch: %s already exists", group.display)
	}
	if !errors.Is(err, fs.ErrNotExist) {
		return nil, fmt.Errorf("patch: cannot inspect %s: %w", group.display, err)
	}
	return &stagedFile{
		path: group.display, absolute: group.absolute, after: []byte(item.body),
		mode: 0o644, sound: true,
	}, nil
}

func stageDelete(ctx context.Context, group fileGroup, env tool.Env) (*stagedFile, error) {
	item := group.edits[0]
	before, info, err := readTarget(ctx, env, group.absolute, group.display)
	if err != nil {
		return nil, err
	}
	if item.reference != "" {
		if err := proveUnchanged(env.Snapshots, item.reference, group.display, before); err != nil {
			return nil, err
		}
	}
	return &stagedFile{
		path: group.display, absolute: group.absolute, before: before,
		exists: true, deleted: true, mode: info.mode, sound: true,
	}, nil
}

func validateChangeShapes(group fileGroup) error {
	last := len(group.edits) - 1
	changes, whole := 0, false
	for i, item := range group.edits {
		if err := checkShape(group.display, item, i == last); err != nil {
			return err
		}
		if item.kind == editChange {
			changes++
			whole = whole || item.locator.kind == locatorWhole
		}
	}
	if whole && changes > 1 {
		return fmt.Errorf("patch: operations for %s cannot combine", group.display)
	}
	return nil
}

func checkShape(display string, item edit, last bool) error {
	switch item.kind {
	case editChange:
		return nil
	case editRename:
		if !last {
			return fmt.Errorf("patch: rename must be the last operation for %s", display)
		}
		return nil
	case editCreate, editDelete:
	}
	return fmt.Errorf("patch: changes[%d]: create or delete cannot combine with other operations for %s", item.index, display)
}

func readTarget(ctx context.Context, env tool.Env, absolute, display string) ([]byte, targetInfo, error) {
	info, err := env.Runner.Stat(ctx, absolute)
	if err != nil {
		return nil, targetInfo{}, fmt.Errorf("patch: %s could not be read: %w", display, err)
	}
	if !info.Mode.IsRegular() {
		return nil, targetInfo{}, fmt.Errorf("patch: %s is not a regular file", display)
	}
	data, err := env.Runner.ReadFile(ctx, absolute)
	if err != nil {
		return nil, targetInfo{}, fmt.Errorf("patch: %s could not be read: %w", display, err)
	}
	return data, targetInfo{mode: info.Mode.Perm()}, nil
}

type targetInfo struct{ mode fs.FileMode }

func checkRenameTargets(files []*stagedFile) error {
	claimed := make(map[string]string)
	for _, file := range files {
		claimed[file.absolute] = file.path
	}
	for _, file := range files {
		if file.renameTo == "" {
			continue
		}
		if owner, ok := claimed[file.renameAbs]; ok {
			return fmt.Errorf("patch: cannot rename %s to %s: another change also writes %s", file.path, file.renameTo, owner)
		}
		claimed[file.renameAbs] = file.renameTo
	}
	return nil
}

func (f *stagedFile) publishPin() *dirPin {
	if f.renamePin != nil {
		return f.renamePin
	}
	return f.pin
}

func (f *stagedFile) closePins() {
	if f == nil {
		return
	}
	f.pin.close()
	f.renamePin.close()
}

func (b stagedBatch) closePins() {
	closeFiles(b.files)
}

func closeFiles(files []*stagedFile) {
	for _, file := range files {
		file.closePins()
	}
}

func (b stagedBatch) changes() []tool.FileChange {
	var out []tool.FileChange
	for _, file := range b.files {
		postPresent := !file.deleted && file.renameTo == ""
		var post []byte
		if postPresent {
			post = file.after
		}
		out = append(out, tool.FileChange{
			Path: file.path, PreSHA: fileSHA(file.before, file.exists),
			PostSHA: fileSHA(file.after, postPresent), Pre: file.before, Post: post,
		})
		if file.renameTo != "" {
			out = append(out, tool.FileChange{
				Path: file.renameTo, PostSHA: snapshot.Digest(file.after), Post: file.after,
			})
		}
	}
	return out
}

func fileSHA(data []byte, present bool) string {
	if !present {
		return ""
	}
	return snapshot.Digest(data)
}
