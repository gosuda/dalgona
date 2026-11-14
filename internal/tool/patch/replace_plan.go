package patch

import (
	"bytes"
	"context"
	"fmt"

	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type ReplacedFile struct {
	Path    string
	Before  []byte
	After   []byte
	Splices []snapshot.Splice
}

func PlanReplace(ctx context.Context, env tool.Env, summary string, replaced []ReplacedFile) (tool.Plan, error) {
	files, err := stageReplacedFiles(ctx, env, replaced)
	if err != nil {
		return tool.Plan{}, err
	}
	if err = checkOmissions(files); err != nil {
		closeFiles(files)
		return tool.Plan{}, err
	}
	batch := stagedBatch{style: Replace, files: files, summary: summary}
	return tool.Plan{
		Files: batch.changes(),
		Apply: func(ctx context.Context) (tool.Result, error) {
			return applyBatch(ctx, env, batch)
		},
		Discard: batch.closePins,
	}, nil
}

func stageReplacedFiles(ctx context.Context, env tool.Env, replaced []ReplacedFile) ([]*stagedFile, error) {
	files := make([]*stagedFile, 0, len(replaced))
	stagedBytes := 0
	for _, item := range replaced {
		file, err := stageReplaced(ctx, env, item)
		if err != nil {
			closeFiles(files)
			return nil, err
		}
		if file == nil {
			continue
		}
		stagedBytes += len(file.before) + len(file.after)
		if err = checkStagedBytes(stagedBytes); err != nil {
			closeFiles(files)
			return nil, err
		}
		if file.pin, err = pinVerifiedComponents(env, file.path, file.absolute); err != nil {
			file.closePins()
			closeFiles(files)
			return nil, err
		}
		files = append(files, file)
	}
	return files, nil
}

func stageReplaced(ctx context.Context, env tool.Env, item ReplacedFile) (*stagedFile, error) {
	display, absolute, err := resolveEnvPath(ctx, env, item.Path)
	if err != nil {
		return nil, err
	}
	current, mode, err := readTarget(ctx, env, absolute, display)
	if err != nil {
		return nil, err
	}
	if !bytes.Equal(current, item.Before) {
		return nil, fmt.Errorf("patch: %s changed while the rewrite was planned. Nothing was written. Try again", display)
	}
	if bytes.Equal(item.Before, item.After) {
		return nil, nil
	}
	return &stagedFile{
		path: display, absolute: absolute, before: current, after: item.After,
		exists: true, mode: mode, splices: item.Splices, sound: true,
	}, nil
}
