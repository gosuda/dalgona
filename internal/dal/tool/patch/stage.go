package patch

import (
	"bytes"
	"cmp"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"path/filepath"
	"regexp"
	"slices"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
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
	beforeSHA string
	afterSHA  string
	splices   []snapshot.Splice
	sound     bool
}

type stagedBatch struct {
	files       []*stagedFile
	summary     string
	style       Style
	version     uint64
	cutoff      snapshot.Evidence
	findings    []observerFinding
	fingerprint [sha256.Size]byte
}

type fileGroup struct {
	display  string
	absolute string
	edits    []edit
}

func stage(ctx context.Context, style Style, edits []edit, env tool.Env, input string) (stagedBatch, error) {
	groups, err := checkedGroups(ctx, env, edits)
	if err != nil {
		return stagedBatch{}, err
	}
	files, err := stageGroups(ctx, style, groups, env)
	if err != nil {
		return stagedBatch{}, err
	}
	return finishBatch(style, env, input, files)
}

func checkedGroups(ctx context.Context, env tool.Env, edits []edit) ([]fileGroup, error) {
	groups, err := groupEdits(ctx, env, edits)
	if err != nil {
		return nil, err
	}
	if len(groups) > 64 {
		return nil, fmt.Errorf("patch: this call touches %d files; the limit is 64", len(groups))
	}
	err = validateGroups(groups, env.Cwd)
	if err != nil {
		return nil, err
	}
	return groups, nil
}

func finishBatch(style Style, env tool.Env, input string, files []*stagedFile) (stagedBatch, error) {
	batch := newStagedBatch(style, env, input)
	for _, file := range files {
		file.beforeSHA = fileSHA(file.before, file.exists)
		file.afterSHA = fileSHA(file.after, !file.deleted)
	}
	batch.files = files
	batch.findings = observeOmissions(files)
	if err := blockingFinding(batch.findings); err != nil {
		closeFiles(files)
		return batch, err
	}
	if len(files) == 0 && !style.profile() {
		return stagedBatch{}, errors.New("patch: the edits produce no change")
	}
	if err := checkRenameTargets(files); err != nil {
		closeFiles(files)
		return stagedBatch{}, err
	}
	return batch, nil
}

func newStagedBatch(style Style, env tool.Env, input string) stagedBatch {
	return stagedBatch{
		style: style, version: 1, cutoff: env.Evidence,
		fingerprint: invocationFingerprint(style, env, input),
	}
}

func invocationFingerprint(style Style, env tool.Env, input string) [sha256.Size]byte {
	hash := sha256.New()
	var size [8]byte
	for _, part := range []string{string(style), "1", env.Cwd, env.Evidence.Consumer, input} {
		binary.BigEndian.PutUint64(size[:], uint64(len(part)))
		_, _ = hash.Write(size[:])
		_, _ = io.WriteString(hash, part)
	}
	var digest [sha256.Size]byte
	hash.Sum(digest[:0])
	return digest
}

func validateGroups(groups []fileGroup, cwd string) error {
	claimed := make(map[string]string, len(groups))
	for _, group := range groups {
		claimed[group.display] = group.display
		if len(group.edits) == 1 && (group.edits[0].kind == editCreate || group.edits[0].kind == editDelete) {
			continue
		}
		if err := validateChangeShapes(group); err != nil {
			return err
		}
	}
	return validateRenameGroups(groups, cwd, claimed)
}

func validateRenameGroups(groups []fileGroup, cwd string, claimed map[string]string) error {
	for _, group := range groups {
		for _, item := range group.edits {
			if item.kind != editRename {
				continue
			}
			if err := validateRenameTarget(item, group, cwd, claimed); err != nil {
				return err
			}
		}
	}
	return nil
}

func validateRenameTarget(item edit, group fileGroup, cwd string, claimed map[string]string) error {
	display, _, err := resolvePatchPath(cwd, item.to)
	if err != nil {
		return err
	}
	if hasParentComponent(item.to) {
		return errors.New("patch: rename destination contains parent traversal")
	}
	if owner, ok := claimed[display]; ok {
		return fmt.Errorf("patch: cannot rename %s to %s: another change also writes %s", group.display, display, owner)
	}
	claimed[display] = display
	return nil
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
		if err = checkStagedBytes(stagedBytes); err != nil {
			file.closePins()
			closeFiles(files)
			return nil, err
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

func checkStagedBytes(total int) error {
	if total > 64<<20 {
		return errors.New("patch: this call stages over 64 MiB; split it into several calls")
	}
	return nil
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
	slices.SortFunc(groups, func(a, b fileGroup) int { return cmp.Compare(a.display, b.display) })
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
	idx := slices.IndexFunc(entries, func(e runner.FileInfo) bool { return e.Name == name })
	if idx < 0 {
		return runner.FileInfo{}, false
	}
	return entries[idx], true
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
	before, mode, err := readTarget(ctx, env, group.absolute, group.display)
	if err != nil {
		return nil, err
	}
	doc, err := decodeDocument(group.display, before)
	if err != nil {
		return nil, err
	}
	file := &stagedFile{
		path: group.display, absolute: group.absolute, before: before,
		exists: true, mode: mode, sound: true,
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
	before, mode, err := readTarget(ctx, env, group.absolute, group.display)
	if err != nil {
		return nil, err
	}
	if item.reference != "" {
		if err := proveUnchanged(env, item.reference, group.display, before); err != nil {
			return nil, err
		}
	}
	return &stagedFile{
		path: group.display, absolute: group.absolute, before: before,
		exists: true, deleted: true, mode: mode, sound: true,
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

func readTarget(ctx context.Context, env tool.Env, absolute, display string) ([]byte, fs.FileMode, error) {
	info, err := env.Runner.Stat(ctx, absolute)
	if err != nil {
		return nil, 0, fmt.Errorf("patch: %s could not be read: %w", display, err)
	}
	if !info.Mode.IsRegular() {
		return nil, 0, fmt.Errorf("patch: %s is not a regular file", display)
	}
	data, err := env.Runner.ReadFile(ctx, absolute)
	if err != nil {
		return nil, 0, fmt.Errorf("patch: %s could not be read: %w", display, err)
	}
	return data, info.Mode.Perm(), nil
}

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
			post = bytes.Clone(file.after)
		}
		out = append(out, tool.FileChange{
			Path: file.path, PreSHA: fileSHA(file.before, file.exists),
			PostSHA: fileSHA(file.after, postPresent), Pre: bytes.Clone(file.before), Post: post,
		})
		if file.renameTo != "" {
			out = append(out, tool.FileChange{
				Path: file.renameTo, PostSHA: snapshot.Digest(file.after), Post: bytes.Clone(file.after),
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
