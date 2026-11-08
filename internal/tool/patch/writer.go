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
	"strings"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type writeState struct {
	file   *stagedFile
	temp   string
	backup string
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
		rollbackFailures := rollback(ctx, env, effects)
		cleanupStates(ctx, env, states, len(rollbackFailures) == 0)
		return tool.Result{}, writeError(failed, commitErr, states, rollbackFailures)
	}
	cleanupStates(ctx, env, states, true)
	refs := recordSnapshots(env, batch.files)
	return tool.Result{Output: appliedOutput(batch.files, refs), OK: true}, nil
}

func verifyBatch(ctx context.Context, env tool.Env, files []*stagedFile) error {
	for _, file := range files {
		if err := verifySource(ctx, env, file); err != nil {
			return err
		}
		if file.renameTo != "" {
			if err := verifyAbsent(ctx, env, file.renameAbs, file.renameTo); err != nil {
				return err
			}
		}
	}
	return nil
}

func verifySource(ctx context.Context, env tool.Env, file *stagedFile) error {
	if !file.exists {
		return verifyAbsent(ctx, env, file.absolute, file.path)
	}
	current, err := env.Runner.ReadFile(ctx, file.absolute)
	if err != nil {
		return fmt.Errorf("patch: cannot read %s before commit: %w", file.path, err)
	}
	if !bytes.Equal(current, file.before) {
		return fmt.Errorf("patch: %s changed while you were approving. Nothing was written. Try again", file.path)
	}
	return nil
}

func verifyAbsent(ctx context.Context, env tool.Env, absolute, display string) error {
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
	if _, ok := run.(*runner.Local); ok {
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
			cleanupStates(ctx, env, states, true)
			return nil, err
		}
		states = append(states, state)
		if err := ctx.Err(); err != nil {
			cleanupStates(ctx, env, states, true)
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
	state := &writeState{file: file}
	target := file.absolute
	if file.renameTo != "" {
		target = file.renameAbs
	}
	if !file.deleted {
		state.temp = target + ".dalgona-tmp-" + suffix
		if err := env.Runner.WriteFile(ctx, state.temp, file.after, file.mode); err != nil {
			return nil, fmt.Errorf("patch: cannot stage %s: %w", file.path, err)
		}
	}
	if file.exists {
		state.backup = file.absolute + ".dalgona-backup-" + suffix
		if err := env.Runner.WriteFile(ctx, state.backup, file.before, file.mode); err != nil {
			if state.temp != "" {
				_ = env.Runner.Remove(ctx, state.temp)
			}
			return nil, fmt.Errorf("patch: cannot back up %s: %w", file.path, err)
		}
	}
	return state, nil
}

func tempSuffix() (string, error) {
	var random [12]byte
	if _, err := rand.Read(random[:]); err != nil {
		return "", fmt.Errorf("patch: allocate temporary name: %w", err)
	}
	return hex.EncodeToString(random[:]), nil
}

func commitWriteSet(ctx context.Context, env tool.Env, states []*writeState) ([]*writeEffect, *writeState, error) {
	var effects []*writeEffect
	for _, state := range states {
		if len(effects) == 0 {
			if err := ctx.Err(); err != nil {
				return effects, state, err
			}
		}
		effect, err := commitOne(ctx, env, state)
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
		if err := env.Runner.Remove(ctx, file.absolute); err != nil {
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
	if err := env.Runner.Remove(ctx, file.absolute); err != nil {
		return effect, fmt.Errorf("patch: cannot remove rename source %s: %w", file.path, err)
	}
	effect.removedSource = true
	return effect, nil
}

func publishExisting(ctx context.Context, env tool.Env, state *writeState, target string) error {
	return env.Runner.WriteFile(ctx, target, state.file.after, state.file.mode)
}

func publishAbsent(ctx context.Context, env tool.Env, state *writeState, target string) error {
	if linker, ok := env.Runner.(linkFileRunner); ok {
		return linker.LinkFile(ctx, state.temp, target)
	}
	if _, ok := env.Runner.(*runner.Local); !ok {
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
	file := effect.state.file
	var failures []error
	if effect.created {
		if err := env.Runner.Remove(ctx, effect.target); err != nil && !errors.Is(err, fs.ErrNotExist) {
			failures = append(failures, err)
		}
	}
	if effect.replaced {
		if err := env.Runner.WriteFile(ctx, effect.target, file.before, file.mode); err != nil {
			failures = append(failures, err)
		}
	}
	if effect.removedSource || file.deleted {
		if err := env.Runner.WriteFile(ctx, file.absolute, file.before, file.mode); err != nil {
			failures = append(failures, err)
		}
	}
	return errors.Join(failures...)
}

func cleanupStates(ctx context.Context, env tool.Env, states []*writeState, backups bool) {
	for _, state := range states {
		if state.temp != "" {
			_ = env.Runner.Remove(ctx, state.temp)
		}
		if backups && state.backup != "" {
			_ = env.Runner.Remove(ctx, state.backup)
		}
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

func appliedOutput(files []*stagedFile, refs map[string]string) []byte {
	var out strings.Builder
	fmt.Fprintf(&out, "patch: applied %d file(s)\n", len(files))
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
