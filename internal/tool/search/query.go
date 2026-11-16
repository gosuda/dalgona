package search

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp/syntax"
	"strconv"
	"time"

	"github.com/google/codesearch/index"
)

const racyWindow = 2 * time.Second

const indexFailedStatus = 1

var errIndex = errors.New("search: index unusable")

type freshness struct {
	Now  int64
	Racy int64
}

type trigramFilter struct {
	manifest manifest
	all      bool
	named    map[string]bool
	clock    freshness
}

var afterGenerationOpened = func(string) {}

func openTrigramFilter(dir, pattern string, clock freshness) (*trigramFilter, error) {
	if dir == "" {
		return nil, nil
	}
	m, trigram, err := openGeneration(dir)
	if err != nil || trigram == nil {
		return nil, err
	}
	defer func() { _ = trigram.Close() }()
	afterGenerationOpened(dir)
	if !generationPinned(dir, trigram, m.TrigramSize) {
		return nil, nil
	}
	re, err := syntax.Parse(pattern, syntax.Perl)
	if err != nil {
		return nil, fmt.Errorf("%w: %w", errIndex, err)
	}
	filter := &trigramFilter{manifest: m, clock: clock}
	query := index.RegexpQuery(re)
	if query.Op == index.QAll {
		filter.all = true
		return filter, nil
	}
	filter.named, err = namedCandidates(trigram, query)
	if err != nil {
		return nil, err
	}
	return filter, nil
}

func openGeneration(dir string) (manifest, *os.File, error) {
	m, err := readManifest(filepath.Join(dir, manifestFile))
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return manifest{}, nil, nil
		}
		return manifest{}, nil, fmt.Errorf("%w: %w", errIndex, err)
	}
	trigram, err := openTrigram(filepath.Join(dir, trigramFile), m.TrigramSize)
	if err != nil || trigram == nil {
		return manifest{}, nil, err
	}
	return m, trigram, nil
}

func openTrigram(path string, size int64) (*os.File, error) {
	info, err := os.Lstat(path) //nolint:gosec // trigram path within the verified generation directory
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, nil
		}
		return nil, fmt.Errorf("%w: %w", errIndex, err)
	}
	if !pinnedTrigramSize(info, size) {
		return nil, fmt.Errorf("%w: trigram size", errIndex)
	}
	file, err := os.Open(path) //nolint:gosec // trigram path within the verified generation directory
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, nil
		}
		return nil, fmt.Errorf("%w: %w", errIndex, err)
	}
	pinned, err := file.Stat()
	if err != nil {
		_ = file.Close()
		return nil, fmt.Errorf("%w: %w", errIndex, err)
	}
	if !pinnedTrigramSize(pinned, size) {
		_ = file.Close()
		return nil, fmt.Errorf("%w: trigram size", errIndex)
	}
	return file, nil
}

func generationPinned(dir string, trigram *os.File, size int64) bool {
	pinned, err := trigram.Stat()
	if err != nil || !pinnedTrigramSize(pinned, size) {
		return false
	}
	current, err := os.Stat(filepath.Join(dir, trigramFile)) //nolint:gosec // generation file compared with its pinned descriptor
	if err != nil {
		return false
	}
	return os.SameFile(pinned, current)
}

func pinnedTrigramSize(info fs.FileInfo, size int64) bool {
	return info.Mode().IsRegular() && info.Size() == size
}

func namedCandidates(trigram *os.File, query *index.Query) (names map[string]bool, err error) {
	defer func() {
		if r := recover(); r != nil {
			names, err = nil, fmt.Errorf("%w: %v", errIndex, r)
		}
	}()
	descriptor, err := descriptorPath(trigram)
	if err != nil {
		return nil, err
	}
	ix := index.Open(descriptor)
	ids := ix.PostingQuery(query)
	names = make(map[string]bool, len(ids))
	for _, id := range ids {
		names[ix.Name(id)] = true
	}
	return names, nil
}

func descriptorPath(file *os.File) (string, error) {
	digits := strconv.Itoa(int(file.Fd()))
	for _, prefix := range []string{"/proc/self/fd/", "/dev/fd/"} {
		path := prefix + digits
		if _, err := os.Stat(path); err == nil {
			return path, nil
		}
	}
	return "", fmt.Errorf("%w: no descriptor path", errIndex)
}

func statMatches(rec manifestEntry, info fs.FileInfo) bool {
	ctime, ok := ctimeNS(info)
	return ok && rec.Size == info.Size() && rec.ModNS == info.ModTime().UnixNano() && rec.CtimeNS == ctime
}

func settled(rec manifestEntry, info fs.FileInfo, m manifest, clock freshness) bool {
	cutoff := m.Start - clock.Racy
	return ctimeSupported && clock.Now >= m.Start && statMatches(rec, info) && rec.ModNS < cutoff && rec.CtimeNS < cutoff
}

func (f *trigramFilter) decide(display string, info fs.FileInfo) (read, counted bool) {
	rec, ok := f.manifest.lookup(display)
	if ok && rec.Indexed && settled(rec, info, f.manifest, f.clock) {
		return f.all || f.named[display], false
	}
	if ok && !rec.Indexed && statMatches(rec, info) {
		return true, false
	}
	return true, true
}

func planReads(files []fileEntry, filter *trigramFilter) ([]fileEntry, int) {
	read := make([]fileEntry, 0, len(files))
	counted := 0
	for _, file := range files {
		info, err := os.Lstat(file.absolute)
		if err != nil || !searchableInfo(info) {
			continue
		}
		doRead, count := true, true
		if filter != nil {
			doRead, count = filter.decide(file.display, info)
		}
		if doRead {
			read = append(read, file)
		}
		if count {
			counted += int(info.Size())
		}
	}
	return read, counted
}

func parseIndexArgs(argv []string) (string, freshness, error) {
	if len(argv) == 0 {
		return "", freshness{}, nil
	}
	if len(argv) != 4 {
		return "", freshness{}, fmt.Errorf("search: index arguments need <root> <generation> <now-ns> <racy-ns>")
	}
	if !singlePathElement(argv[1]) {
		return "", freshness{}, fmt.Errorf("search: invalid generation %q", argv[1])
	}
	now, err := strconv.ParseInt(argv[2], 10, 64)
	if err != nil {
		return "", freshness{}, fmt.Errorf("search: invalid clock %q", argv[2])
	}
	racy, err := strconv.ParseInt(argv[3], 10, 64)
	if err != nil {
		return "", freshness{}, fmt.Errorf("search: invalid racy window %q", argv[3])
	}
	return filepath.Join(argv[0], argv[1]), freshness{Now: now, Racy: racy}, nil
}
