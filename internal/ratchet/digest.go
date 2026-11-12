package ratchet

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"hash"
	"io"
	"io/fs"
	"maps"
	"path/filepath"
	"slices"

	json "encoding/json/v2"
)

const (
	stageInputs = "inputs"
	stageGrader = "grader"
	stagePlan   = "plan"

	maxHashFiles = 10000
)

var stages = []string{stageInputs, stageGrader, stagePlan}

type fileSHA struct {
	Path string `json:"path"`
	SHA  string `json:"sha"`
}

type planDigest struct {
	Goal        goal     `json:"goal"`
	Reps        int      `json:"reps"`
	Stop        stopRule `json:"stop"`
	Command     string   `json:"command"`
	ChangePaths []string `json:"change_paths"`
	OffLimits   []string `json:"off_limits"`
	TrainIDs    []string `json:"train_ids"`
	TestIDs     []string `json:"test_ids"`
	SplitSeed   uint64   `json:"split_seed"`
}

func (f *flow) hashPaths(paths []string) (string, []fileSHA, error) {
	hasher := sha256.New()
	var files []fileSHA
	for _, p := range slices.Sorted(maps.Keys(uniqueSet(paths))) {
		entries, err := f.hashPath(hasher, p)
		if err != nil {
			return "", nil, err
		}
		files = append(files, entries...)
	}
	return hex.EncodeToString(hasher.Sum(nil)), files, nil
}

func uniqueSet(paths []string) map[string]struct{} {
	set := make(map[string]struct{}, len(paths))
	for _, p := range paths {
		set[p] = struct{}{}
	}
	return set
}

func (f *flow) hashPath(hasher hash.Hash, p string) ([]fileSHA, error) {
	_, err := f.root.Stat(filepath.FromSlash(p))
	if errors.Is(err, fs.ErrNotExist) {
		_, _ = io.WriteString(hasher, "missing:"+p+"\x00")
		return []fileSHA{{Path: p}}, nil
	}
	if err != nil {
		return nil, fmt.Errorf("stat %s: %w", p, err)
	}
	rels, err := f.regularFiles(p)
	if err != nil {
		return nil, err
	}
	files := make([]fileSHA, 0, len(rels))
	for _, rel := range rels {
		sum, err := f.hashFile(hasher, rel)
		if err != nil {
			return nil, err
		}
		files = append(files, fileSHA{Path: rel, SHA: sum})
	}
	return files, nil
}

func (f *flow) regularFiles(p string) ([]string, error) {
	var rels []string
	err := fs.WalkDir(f.root.FS(), filepath.ToSlash(p), func(rel string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			return nil
		}
		info, err := f.root.Stat(filepath.FromSlash(rel))
		if err != nil {
			return fmt.Errorf("stat %s: %w", rel, err)
		}
		if !info.Mode().IsRegular() {
			return nil
		}
		if len(rels) == maxHashFiles {
			return fmt.Errorf("%s holds more than %d files", p, maxHashFiles)
		}
		rels = append(rels, rel)
		return nil
	})
	return rels, err
}

func (f *flow) hashFile(hasher hash.Hash, rel string) (string, error) {
	file, err := f.root.Open(filepath.FromSlash(rel))
	if err != nil {
		return "", fmt.Errorf("open %s: %w", rel, err)
	}
	defer func() { _ = file.Close() }()
	own := sha256.New()
	_, _ = io.WriteString(hasher, rel+"\x00")
	if _, err := io.Copy(io.MultiWriter(hasher, own), file); err != nil {
		return "", fmt.Errorf("read %s: %w", rel, err)
	}
	_, _ = io.WriteString(hasher, "\x00")
	return hex.EncodeToString(own.Sum(nil)), nil
}

func (f *flow) digest(s *state, stage string) (string, error) {
	switch stage {
	case stageInputs:
		sum, _, err := f.hashPaths(s.CasesPaths)
		return sum, err
	case stageGrader:
		sum, _, err := f.hashPaths(s.HarnessPaths)
		return sum, err
	case stagePlan:
		return planSum(s)
	}
	return "", fmt.Errorf("unknown stage %q", stage)
}

func planSum(s *state) (string, error) {
	if s.Goal.Target == "" || s.Command == "" || len(s.TrainIDs) == 0 || len(s.TestIDs) == 0 {
		return "", errors.New("plan needs a target, a command and a split first")
	}
	plan := planDigest{
		Goal: goal{
			Target:     s.Goal.Target,
			Direction:  s.Goal.Direction,
			Hold:       nonNil(s.Goal.Hold),
			Directions: nonNilMap(s.Goal.Directions),
		},
		Reps:        s.Reps,
		Stop:        s.Stop,
		Command:     s.Command,
		ChangePaths: nonNil(s.ChangePaths),
		OffLimits:   nonNil(s.OffLimits),
		TrainIDs:    s.TrainIDs,
		TestIDs:     s.TestIDs,
		SplitSeed:   s.SplitSeed,
	}
	data, err := json.Marshal(plan, json.Deterministic(true))
	if err != nil {
		return "", fmt.Errorf("encode plan: %w", err)
	}
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:]), nil
}

func nonNil[T any](values []T) []T {
	if values == nil {
		return []T{}
	}
	return values
}

func nonNilMap(values map[string]string) map[string]string {
	if values == nil {
		return map[string]string{}
	}
	return values
}

func (f *flow) approvals(s *state) (map[string]string, error) {
	status := make(map[string]string, len(stages))
	for _, stage := range stages {
		recorded, ok := s.Approvals[stage]
		if !ok {
			status[stage] = "missing"
			continue
		}
		current, err := f.digest(s, stage)
		if err != nil {
			return nil, err
		}
		status[stage] = "stale"
		if current == recorded.SHA {
			status[stage] = "current"
		}
	}
	return status, nil
}
