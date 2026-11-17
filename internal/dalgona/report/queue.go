package report

import (
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"time"
)

const (
	reportDirName    = "failure-reporting"
	queueFileName    = "queue.json"
	stateFileName    = "state.json"
	maxQueueEntries  = 64
	maxQueueBytes    = 1 << 20
	maxReceipts      = 100000
	maxStateBytes    = 16 << 20
	maxSearchPages   = 10
	maxDayCreates    = 3
	maxRunCreates    = 1
	maxDayReactions  = 3
	maxRunReactions  = 1
	maxDayRests      = 20
	maxRunRests      = 10
	writeSpacing     = 60 * time.Second
	queueEntryMaxAge = 7 * 24 * time.Hour
)

const (
	receiptAttempted = "attempted"
	receiptFiled     = "filed"
	receiptFound     = "found"
	receiptUncertain = "uncertain"
)

type queueFile struct {
	Entries []QueueEntry `json:"entries"`
}

type reporterState struct {
	Receipts         []Receipt `json:"receipts"`
	Dropped          uint64    `json:"dropped"`
	CreatesDay       string    `json:"createsDay"`
	Creates          int       `json:"creates"`
	ReactionsDay     string    `json:"reactionsDay"`
	Reactions        int       `json:"reactions"`
	RestsDay         string    `json:"restsDay"`
	Rests            int       `json:"rests"`
	LastWrite        time.Time `json:"lastWrite"`
	SearchRemaining  int       `json:"searchRemaining"`
	SearchReset      time.Time `json:"searchReset"`
	SearchRetryAfter time.Time `json:"searchRetryAfter"`
	CoreRemaining    int       `json:"coreRemaining"`
	CoreReset        time.Time `json:"coreReset"`
	CoreRetryAfter   time.Time `json:"coreRetryAfter"`
	StoppedStatus    int       `json:"stoppedStatus"`
}

func ensureReporterDir(dir string) error {
	info, err := os.Lstat(dir)
	if errors.Is(err, fs.ErrNotExist) {
		return createReporterDir(dir)
	}
	if err != nil {
		return fmt.Errorf("failure reporting directory is not private: %w", err)
	}
	return checkReporterDir(info)
}

func createReporterDir(dir string) error {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return fmt.Errorf("failure reporting directory is not private: %w", err)
	}
	info, err := os.Lstat(dir)
	if err != nil {
		return fmt.Errorf("failure reporting directory is not private: %w", err)
	}
	return checkReporterDir(info)
}

func checkReporterDir(info fs.FileInfo) error {
	if info.Mode()&fs.ModeSymlink != 0 || !info.IsDir() {
		return errors.New("failure reporting directory is not private")
	}
	if runtime.GOOS == "windows" {
		return nil
	}
	if info.Mode().Perm()&0o077 != 0 {
		return errors.New("failure reporting directory is not private")
	}
	if stat, ok := info.Sys().(interface{ Uid() uint32 }); ok {
		if int(stat.Uid()) != os.Getuid() {
			return errors.New("failure reporting directory is not private")
		}
	}
	return nil
}

func atomicWriteFile(dir, name string, data []byte) error {
	staging, err := os.CreateTemp(dir, ".pending-report-*")
	if err != nil {
		return fmt.Errorf("failure reporting write local state: %w", err)
	}
	stagingName := staging.Name()
	if _, err := staging.Write(data); err != nil {
		_ = staging.Close()
		_ = os.Remove(stagingName)
		return fmt.Errorf("failure reporting write local state: %w", err)
	}
	if err := staging.Sync(); err != nil {
		_ = staging.Close()
		_ = os.Remove(stagingName)
		return fmt.Errorf("failure reporting write local state: %w", err)
	}
	if err := staging.Close(); err != nil {
		_ = os.Remove(stagingName)
		return fmt.Errorf("failure reporting write local state: %w", err)
	}
	if err := os.Chmod(stagingName, 0o600); err != nil {
		_ = os.Remove(stagingName)
		return fmt.Errorf("failure reporting write local state: %w", err)
	}
	if err := os.Rename(stagingName, filepath.Join(dir, name)); err != nil {
		_ = os.Remove(stagingName)
		return fmt.Errorf("failure reporting write local state: %w", err)
	}
	syncReporterParent(dir)
	return nil
}

func syncReporterParent(dir string) {
	parent, err := os.Open(dir)
	if err != nil {
		return
	}
	defer func() { _ = parent.Close() }()
	_ = parent.Sync()
}

func readQueueFile(dir string) ([]QueueEntry, error) {
	raw, err := os.ReadFile(filepath.Join(dir, queueFileName))
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, nil
		}
		return nil, fmt.Errorf("failure reporting read local state: %w", err)
	}
	var decoded queueFile
	if err := json.Unmarshal(raw, &decoded); err != nil {
		return nil, errors.New("failure reporting stopped: state is invalid")
	}
	return decoded.Entries, nil
}

func readStateFile(dir string) (reporterState, error) {
	var state reporterState
	raw, err := os.ReadFile(filepath.Join(dir, stateFileName))
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return state, nil
		}
		return state, fmt.Errorf("failure reporting read local state: %w", err)
	}
	if err := json.Unmarshal(raw, &state); err != nil {
		return state, errors.New("failure reporting stopped: state is invalid")
	}
	return state, nil
}
