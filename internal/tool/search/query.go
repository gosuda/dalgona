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

func openTrigramFilter(dir, pattern string, clock freshness) (*trigramFilter, error) {
	if dir == "" {
		return nil, nil
	}
	present, err := generationPresent(dir)
	if err != nil || !present {
		return nil, err
	}
	m, err := loadGeneration(dir)
	if err != nil {
		return nil, err
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
	filter.named, err = namedCandidates(filepath.Join(dir, trigramFile), query)
	if err != nil {
		return nil, err
	}
	return filter, nil
}

func loadGeneration(dir string) (manifest, error) {
	m, err := readManifest(filepath.Join(dir, manifestFile))
	if err != nil {
		return manifest{}, fmt.Errorf("%w: %w", errIndex, err)
	}
	info, err := os.Stat(filepath.Join(dir, trigramFile)) //nolint:gosec // trigram index path within verified generation directory
	if err != nil {
		return manifest{}, fmt.Errorf("%w: %w", errIndex, err)
	}
	if info.Size() != m.TrigramSize {
		return manifest{}, fmt.Errorf("%w: trigram size", errIndex)
	}
	return m, nil
}

func generationPresent(dir string) (bool, error) {
	for _, name := range []string{manifestFile, trigramFile} {
		if _, err := os.Stat(filepath.Join(dir, name)); err != nil { //nolint:gosec // generation files checked within verified directory
			if errors.Is(err, fs.ErrNotExist) {
				return false, nil
			}
			return false, fmt.Errorf("%w: %w", errIndex, err)
		}
	}
	return true, nil
}

func namedCandidates(trigram string, query *index.Query) (names map[string]bool, err error) {
	defer func() {
		if r := recover(); r != nil {
			names, err = nil, fmt.Errorf("%w: %v", errIndex, r)
		}
	}()
	ix := index.Open(trigram)
	ids := ix.PostingQuery(query)
	names = make(map[string]bool, len(ids))
	for _, id := range ids {
		names[ix.Name(id)] = true
	}
	return names, nil
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
