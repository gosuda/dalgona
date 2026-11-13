package search

import (
	"bytes"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log" //nolint:depguard // the codesearch library writes through package log; buildIndex routes that output
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/google/codesearch/index"
)

const (
	maxBuildBytes = 1 << 30
	currentFile   = "CURRENT"
	trigramFile   = "trigram"
	manifestFile  = "manifest"
	buildUsage    = "usage: __search-index <root> <generation> <start-ns>"
)

var errBuildUsage = errors.New(buildUsage)

func BuildMain(argv []string, stdout io.Writer) int {
	args, err := parseBuildArgs(argv)
	if errors.Is(err, errBuildUsage) {
		_, _ = fmt.Fprint(stdout, buildUsage)
		return 2
	}
	if err != nil {
		_, _ = fmt.Fprint(stdout, err)
		return 2
	}
	if !ctimeSupported {
		_, _ = fmt.Fprint(stdout, "search: index needs change times on this platform")
		return 1
	}
	cwd, err := os.Getwd()
	if err != nil {
		_, _ = fmt.Fprint(stdout, err)
		return 1
	}
	if err := buildIndex(cwd, filepath.Join(args.root, args.gen), args.start, maxBuildBytes); err != nil {
		_, _ = fmt.Fprint(stdout, err)
		return 1
	}
	return 0
}

type buildArgs struct {
	root  string
	gen   string
	start time.Time
}

func parseBuildArgs(argv []string) (buildArgs, error) {
	if len(argv) != 3 {
		return buildArgs{}, errBuildUsage
	}
	if !singlePathElement(argv[1]) {
		return buildArgs{}, fmt.Errorf("search: invalid generation %q", argv[1])
	}
	startNS, err := strconv.ParseInt(argv[2], 10, 64)
	if err != nil {
		return buildArgs{}, fmt.Errorf("search: invalid start %q", argv[2])
	}
	return buildArgs{root: argv[0], gen: argv[1], start: time.Unix(0, startNS)}, nil
}

func singlePathElement(name string) bool {
	return name != "" && name != "." && name != ".." && !strings.ContainsAny(name, "/\\\x00")
}

type libraryLog struct{}

var libraryNotices = [][]byte{[]byte(" index bytes\n"), []byte(" files + mem\n")}

func (libraryLog) Write(p []byte) (int, error) {
	for _, notice := range libraryNotices {
		if bytes.HasSuffix(p, notice) {
			return len(p), nil
		}
	}
	return os.Stderr.Write(p)
}

func buildIndex(cwd, dir string, start time.Time, limit int64) error {
	entries, err := walkScope(cwd, cwd)
	if err != nil {
		return err
	}
	previous := log.Writer()
	log.SetOutput(libraryLog{})
	defer log.SetOutput(previous)

	trigramPath := filepath.Join(dir, trigramFile)
	writer := index.Create(trigramPath)
	records := make([]manifestEntry, 0, len(entries))
	var total int64
	for _, entry := range entries {
		rec, ok, added := recordFile(writer, entry, total, limit)
		if !ok {
			continue
		}
		total += added
		records = append(records, rec)
	}
	writer.Flush()
	names, err := indexedNames(dir)
	if err != nil {
		return err
	}
	for i := range records {
		records[i].Indexed = names[records[i].Path]
	}
	if err = syncFile(trigramPath); err != nil {
		return err
	}
	info, err := os.Stat(trigramPath)
	if err != nil {
		return err
	}
	m := manifest{Start: start.UnixNano(), TrigramSize: info.Size(), Entries: records}
	if err = writeManifest(filepath.Join(dir, manifestFile), m); err != nil {
		return err
	}
	return syncDir(dir)
}

func recordFile(writer *index.IndexWriter, entry fileEntry, total, limit int64) (manifestEntry, bool, int64) {
	info, err := os.Lstat(entry.absolute)
	if err != nil || !searchableInfo(info) {
		return manifestEntry{}, false, 0
	}
	ctime, ok := ctimeNS(info)
	if !ok {
		return manifestEntry{}, false, 0
	}
	rec := manifestEntry{Path: entry.display, Size: info.Size(), ModNS: info.ModTime().UnixNano(), CtimeNS: ctime}
	if total+info.Size() > limit {
		return rec, true, 0
	}
	data, ok := readSearchable(entry.absolute)
	if !ok {
		return rec, true, 0
	}
	writer.Add(entry.display, bytes.NewReader(data))
	return rec, true, int64(len(data))
}

func indexedNames(dir string) (map[string]bool, error) {
	path := filepath.Join(dir, trigramFile)
	if _, err := os.Stat(path); err != nil {
		return nil, err
	}
	ix := index.Open(path)
	ids := ix.PostingQuery(&index.Query{Op: index.QAll})
	names := make(map[string]bool, len(ids))
	for _, id := range ids {
		names[ix.Name(id)] = true
	}
	return names, nil
}

func createGeneration(root string, start time.Time) (string, string, error) {
	if err := os.MkdirAll(root, 0o700); err != nil {
		return "", "", err
	}
	gen, err := generationName(start)
	if err != nil {
		return "", "", err
	}
	dir := filepath.Join(root, gen)
	if err := os.Mkdir(dir, 0o700); err != nil {
		return "", "", err
	}
	return gen, dir, nil
}

func generationName(start time.Time) (string, error) {
	var raw [8]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("search: generation name: %w", err)
	}
	return fmt.Sprintf("%020d-%s", start.UnixNano(), hex.EncodeToString(raw[:])), nil
}

func publishGeneration(root, gen string) error {
	file, err := os.CreateTemp(root, "CURRENT-*.tmp")
	if err != nil {
		return err
	}
	name := file.Name()
	if err := writeSynced(file, gen+"\n"); err != nil {
		return errors.Join(err, os.Remove(name))
	}
	if err := os.Rename(name, filepath.Join(root, currentFile)); err != nil {
		return errors.Join(err, os.Remove(name))
	}
	return syncDir(root)
}

func writeSynced(file *os.File, text string) error {
	_, writeErr := file.WriteString(text)
	syncErr := file.Sync()
	return errors.Join(writeErr, syncErr, file.Close())
}

func removeOtherGenerations(root, keep string) error {
	entries, err := os.ReadDir(root)
	if err != nil {
		return err
	}
	var errs []error
	for _, entry := range entries {
		if entry.IsDir() && entry.Name() != keep {
			errs = append(errs, os.RemoveAll(filepath.Join(root, entry.Name())))
		}
	}
	return errors.Join(errs...)
}

func syncDir(path string) error {
	dir, err := os.Open(path)
	if err != nil {
		return err
	}
	syncErr := dir.Sync()
	return errors.Join(syncErr, dir.Close())
}
