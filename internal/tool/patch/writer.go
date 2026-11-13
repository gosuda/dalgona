package patch

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type writeState struct {
	file   *stagedFile
	temp   string
	backup string
	dest   *dirPin
	src    *dirPin
}

type writeEffect struct {
	state         *writeState
	target        string
	created       bool
	replaced      bool
	removedSource bool
}

func applyBatch(ctx context.Context, env tool.Env, batch stagedBatch) (tool.Result, error) {
	if len(batch.files) == 0 {
		return tool.Result{Output: []byte("no_change\n"), OK: true}, nil
	}
	defer batch.closePins()
	if err := verifyBatch(ctx, env, batch.files); err != nil {
		return tool.Result{}, err
	}
	if err := checkNoReplaceSupport(env, batch.files); err != nil {
		return tool.Result{}, err
	}
	states, err := stageWriteSet(ctx, env, batch.files)
	if err != nil {
		return tool.Result{}, err
	}
	effects, failed, commitErr := commitWriteSet(ctx, env, states)
	if commitErr != nil {
		commitCtx := context.WithoutCancel(ctx)
		rollbackFailures := rollback(commitCtx, env, effects)
		if len(rollbackFailures) == 0 {
			cleanupStates(commitCtx, env, states)
		} else {
			cleanupStatesKeepBackups(commitCtx, env, states)
		}
		return tool.Result{}, writeError(failed, commitErr, states, rollbackFailures)
	}
	cleanupStates(context.WithoutCancel(ctx), env, states)
	refs := recordSnapshots(env, batch.files)
	return tool.Result{Output: appliedOutput(batch.summary, batch.files, refs), OK: true}, nil
}

func verifyBatch(ctx context.Context, env tool.Env, files []*stagedFile) error {
	for _, file := range files {
		if err := verifySource(ctx, env, file); err != nil {
			return err
		}
		if file.renameTo != "" {
			if err := verifyAbsent(ctx, env, file.renamePin, file.renameAbs, file.renameTo); err != nil {
				return err
			}
		}
	}
	return nil
}

func verifySource(ctx context.Context, env tool.Env, file *stagedFile) error {
	if !file.exists {
		return verifyAbsent(ctx, env, file.pin, file.absolute, file.path)
	}
	current, err := readPinned(ctx, env, file.pin, file.absolute)
	if err != nil {
		return fmt.Errorf("patch: cannot read %s before commit: %w", file.path, err)
	}
	if !bytes.Equal(current, file.before) {
		return fmt.Errorf("patch: %s changed while you were approving. Nothing was written. Try again", file.path)
	}
	return nil
}

func readPinned(ctx context.Context, env tool.Env, pin *dirPin, path string) ([]byte, error) {
	if pin != nil {
		return pin.readFileAt(filepath.Base(path))
	}
	return env.Runner.ReadFile(ctx, path)
}

func verifyAbsent(ctx context.Context, env tool.Env, pin *dirPin, absolute, display string) error {
	if pin != nil {
		return verifyAbsentAt(pin, absolute, display)
	}
	return verifyAbsentPath(ctx, env, absolute, display)
}

func verifyAbsentAt(pin *dirPin, absolute, display string) error {
	exists, err := pin.leafExistsAfterChain(filepath.Base(absolute), display)
	if err != nil {
		return fmt.Errorf("patch: cannot inspect %s: %w", display, err)
	}
	if exists {
		return fmt.Errorf("patch: %s already exists", display)
	}
	return nil
}

func verifyAbsentPath(ctx context.Context, env tool.Env, absolute, display string) error {
	_, err := env.Runner.Stat(ctx, absolute)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err == nil {
		return fmt.Errorf("patch: %s already exists", display)
	}
	return fmt.Errorf("patch: cannot inspect %s: %w", display, err)
}

type linkFileRunner interface {
	LinkFile(ctx context.Context, oldpath, newpath string) error
}

func supportsNoReplace(run runner.Runner) bool {
	if tool.IsLocalRunner(run) {
		return true
	}
	_, ok := run.(linkFileRunner)
	return ok
}

func checkNoReplaceSupport(env tool.Env, files []*stagedFile) error {
	if supportsNoReplace(env.Runner) {
		return nil
	}
	for _, file := range files {
		if !file.exists || file.renameTo != "" {
			return errors.New("patch: no_replace_unavailable on this runner")
		}
	}
	return nil
}

func stageWriteSet(ctx context.Context, env tool.Env, files []*stagedFile) ([]*writeState, error) {
	states := make([]*writeState, 0, len(files))
	for _, file := range files {
		state, err := stageOneWrite(ctx, env, file)
		if err != nil {
			cleanupStates(ctx, env, states)
			return nil, err
		}
		states = append(states, state)
		if err := ctx.Err(); err != nil {
			cleanupStates(ctx, env, states)
			return nil, err
		}
	}
	return states, nil
}

func stageOneWrite(ctx context.Context, env tool.Env, file *stagedFile) (*writeState, error) {
	suffix, err := tempSuffix()
	if err != nil {
		return nil, err
	}
	state := &writeState{file: file, dest: file.publishPin(), src: file.pin}
	target := file.absolute
	if file.renameTo != "" {
		target = file.renameAbs
	}
	if !file.deleted {
		if err := state.dest.mkdirChain(file.path, 0o700); err != nil {
			state.closePins()
			return nil, err
		}
		state.temp = target + ".dalgona-tmp-" + suffix
		if err := state.stageWrite(ctx, env, state.dest, state.temp, file.after); err != nil {
			state.closePins()
			return nil, fmt.Errorf("patch: cannot stage %s: %w", file.path, err)
		}
	}
	if file.exists {
		state.backup = file.absolute + ".dalgona-backup-" + suffix
		if err := state.stageWrite(ctx, env, state.src, state.backup, file.before); err != nil {
			if state.temp != "" {
				_ = state.removeAt(ctx, env, state.dest, state.temp)
			}
			state.closePins()
			return nil, fmt.Errorf("patch: cannot back up %s: %w", file.path, err)
		}
	}
	return state, nil
}

func (s *writeState) stageWrite(ctx context.Context, env tool.Env, pin *dirPin, path string, data []byte) error {
	if pin != nil {
		return pin.writeFileAt(filepath.Base(path), data, s.file.mode)
	}
	return env.Runner.WriteFile(ctx, path, data, s.file.mode)
}

func (s *writeState) removeAt(ctx context.Context, env tool.Env, pin *dirPin, path string) error {
	if pin != nil {
		return pin.unlinkFileAt(filepath.Base(path))
	}
	return env.Runner.Remove(ctx, path)
}

func (s *writeState) closePins() {
	s.dest.close()
	s.src.close()
}

func tempSuffix() (string, error) {
	var random [12]byte
	if _, err := rand.Read(random[:]); err != nil {
		return "", fmt.Errorf("patch: allocate temporary name: %w", err)
	}
	return hex.EncodeToString(random[:]), nil
}

func commitWriteSet(ctx context.Context, env tool.Env, states []*writeState) ([]*writeEffect, *writeState, error) {
	commitCtx := context.WithoutCancel(ctx)
	var effects []*writeEffect
	for _, state := range states {
		if len(effects) == 0 {
			if err := ctx.Err(); err != nil {
				return effects, state, err
			}
		}
		effect, err := commitOne(commitCtx, env, state)
		if err != nil {
			if effect != nil {
				effects = append(effects, effect)
			}
			return effects, state, err
		}
		effects = append(effects, effect)
	}
	return effects, nil, nil
}

func commitOne(ctx context.Context, env tool.Env, state *writeState) (*writeEffect, error) {
	file := state.file
	if file.deleted {
		if err := state.removeAt(ctx, env, state.src, file.absolute); err != nil {
			return nil, fmt.Errorf("patch: cannot delete %s: %w", file.path, err)
		}
		return &writeEffect{state: state, target: file.absolute, removedSource: true}, nil
	}
	if file.renameTo != "" {
		return commitRename(ctx, env, state)
	}
	publish := publishExisting
	if !file.exists {
		publish = publishAbsent
	}
	if err := publish(ctx, env, state, file.absolute); err != nil {
		return nil, fmt.Errorf("patch: cannot write %s: %w", file.absolute, err)
	}
	return &writeEffect{state: state, target: file.absolute, created: !file.exists, replaced: file.exists}, nil
}

func commitRename(ctx context.Context, env tool.Env, state *writeState) (*writeEffect, error) {
	file := state.file
	if err := publishAbsent(ctx, env, state, file.renameAbs); err != nil {
		return nil, fmt.Errorf("patch: cannot write %s: %w", file.renameAbs, err)
	}
	effect := &writeEffect{state: state, target: file.renameAbs, created: true}
	if err := state.removeAt(ctx, env, state.src, file.absolute); err != nil {
		return effect, fmt.Errorf("patch: cannot remove rename source %s: %w", file.path, err)
	}
	effect.removedSource = true
	return effect, nil
}

func publishExisting(ctx context.Context, env tool.Env, state *writeState, target string) error {
	if state.dest != nil {
		return state.dest.publishOver(filepath.Base(target), state.file.after, state.file.mode)
	}
	return env.Runner.WriteFile(ctx, target, state.file.after, state.file.mode)
}

func publishAbsent(ctx context.Context, env tool.Env, state *writeState, target string) error {
	if state.dest != nil {
		return state.dest.linkFileAt(filepath.Base(state.temp), filepath.Base(target))
	}
	if linker, ok := env.Runner.(linkFileRunner); ok {
		return linker.LinkFile(ctx, state.temp, target)
	}
	if !tool.IsLocalRunner(env.Runner) {
		return errors.New("no_replace_unavailable")
	}
	return os.Link(state.temp, target)
}

func rollback(ctx context.Context, env tool.Env, effects []*writeEffect) []string {
	var failures []string
	for i := len(effects) - 1; i >= 0; i-- {
		effect := effects[i]
		if err := restoreEffect(ctx, env, effect); err != nil {
			failures = append(failures, fmt.Sprintf("%s: %v (backup %s)", effect.target, err, effect.state.backup))
		}
	}
	return failures
}

func restoreEffect(ctx context.Context, env tool.Env, effect *writeEffect) error {
	state := effect.state
	file := state.file
	var failures []error
	if effect.created {
		if err := state.removeAt(ctx, env, state.dest, effect.target); err != nil && !errors.Is(err, fs.ErrNotExist) {
			failures = append(failures, err)
		}
	}
	if effect.replaced {
		if err := state.restoreAt(ctx, env, state.dest, effect.target); err != nil {
			failures = append(failures, err)
		}
	}
	if effect.removedSource || file.deleted {
		if err := state.restoreAt(ctx, env, state.src, file.absolute); err != nil {
			failures = append(failures, err)
		}
	}
	return errors.Join(failures...)
}

func (s *writeState) restoreAt(ctx context.Context, env tool.Env, pin *dirPin, path string) error {
	if pin == nil {
		return env.Runner.WriteFile(ctx, path, s.file.before, s.file.mode)
	}
	suffix, err := tempSuffix()
	if err != nil {
		return err
	}
	base := filepath.Base(path)
	temp := base + ".dalgona-restore-" + suffix
	if err := pin.writeFileAt(temp, s.file.before, s.file.mode); err != nil {
		return err
	}
	return pin.renameFileAt(temp, base)
}

func cleanupStates(ctx context.Context, env tool.Env, states []*writeState) {
	for _, state := range states {
		cleanupTempState(ctx, env, state)
		cleanupBackupState(ctx, env, state)
		state.closePins()
	}
}

func cleanupStatesKeepBackups(ctx context.Context, env tool.Env, states []*writeState) {
	for _, state := range states {
		cleanupTempState(ctx, env, state)
		state.closePins()
	}
}

func cleanupTempState(ctx context.Context, env tool.Env, state *writeState) {
	if state.temp != "" {
		_ = state.removeAt(ctx, env, state.dest, state.temp)
	}
}

func cleanupBackupState(ctx context.Context, env tool.Env, state *writeState) {
	if state.backup != "" {
		_ = state.removeAt(ctx, env, state.src, state.backup)
	}
}

func writeError(failed *writeState, cause error, states []*writeState, rollbackFailures []string) error {
	paths := make([]string, 0, len(states)*2)
	for _, state := range states {
		paths = append(paths, state.file.path)
		if state.file.renameTo != "" {
			paths = append(paths, state.file.renameTo)
		}
	}
	var out strings.Builder
	if failed != nil {
		fmt.Fprintf(&out, "patch: commit failed at %s: %v. ", failed.file.path, cause)
	} else {
		fmt.Fprintf(&out, "patch: commit failed: %v. ", cause)
	}
	if len(rollbackFailures) == 0 {
		out.WriteString("All completed writes were restored. ")
	} else {
		out.WriteString("Rollback was incomplete. ")
	}
	fmt.Fprintf(&out, "Affected paths: %s.", strings.Join(paths, ", "))
	for _, failure := range rollbackFailures {
		fmt.Fprintf(&out, "\n%s", failure)
	}
	if len(rollbackFailures) > 0 {
		for _, state := range states {
			if state.backup != "" {
				fmt.Fprintf(&out, "\nRetained backup for %s: %s", state.file.path, state.backup)
			}
		}
	}
	return errors.New(out.String())
}

func recordSnapshots(env tool.Env, files []*stagedFile) map[string]string {
	refs := make(map[string]string)
	if env.Snapshots == nil {
		return refs
	}
	for _, file := range files {
		path := file.path
		switch {
		case file.deleted:
			env.Snapshots.Remove(path, file.before)
			continue
		case file.renameTo != "":
			path = file.renameTo
			refs[path] = env.Snapshots.Rename(file.path, path, file.before, file.after, file.splices)
		case file.sound:
			refs[path] = env.Snapshots.Record(path, file.before, file.after, file.splices)
		default:
			refs[path] = env.Snapshots.Put(path, file.after, nil)
		}
	}
	return refs
}

func appliedOutput(summary string, files []*stagedFile, refs map[string]string) []byte {
	var out strings.Builder
	if summary == "" {
		fmt.Fprintf(&out, "patch: applied %d file(s)\n", len(files))
	} else {
		out.WriteString(summary)
	}
	for _, file := range files {
		path := file.path
		if file.renameTo != "" {
			path = file.renameTo
		}
		if file.deleted {
			fmt.Fprintf(&out, "[%s#DELETED]\n", path)
			continue
		}
		fmt.Fprintf(&out, "[%s#%s]\n", path, snapshot.Tag("whole", file.after))
		writeAppliedLines(&out, file)
		if ref := refs[path]; ref != "" {
			fmt.Fprintf(&out, "[%s@%s]\n", snapshot.HeaderPath(path), ref)
		}
	}
	return []byte(out.String())
}

func writeAppliedLines(out *strings.Builder, file *stagedFile) {
	text := strings.TrimPrefix(string(file.after), "\ufeff")
	text = strings.ReplaceAll(text, "\r\n", "\n")
	lines := strings.Split(text, "\n")
	if len(lines) > 0 && lines[len(lines)-1] == "" {
		lines = lines[:len(lines)-1]
	}
	for i, line := range lines {
		if i == 2000 {
			fmt.Fprintf(out, "[Truncated: %d more lines]\n", len(lines)-i)
			return
		}
		fmt.Fprintf(out, "%d:%s\n", i+1, line)
	}
}
