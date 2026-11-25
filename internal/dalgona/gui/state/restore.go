package state

import (
	"cmp"
	"crypto/sha256"
	"encoding/hex"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/gosuda/dalgona/internal/dalgona/client"
)

const (
	restoreVersion   = 1
	manifestName     = "restore.json"
	draftSuffix      = ".draft"
	manifestLimit    = 1 << 20
	endpointRefLimit = 32
	threadRefLimit   = 32
	draftRefLimit    = 64
	draftFileLimit   = 1 << 20
	draftBudget      = 16 << 20
)

type (
	draftDigest  [sha256.Size]byte
	restoreImage struct {
		windows map[WindowID][]ThreadKey
		drafts  map[DraftKey]Draft
	}
	restoredMsg struct {
		windows  []manifestWindow
		drafts   map[DraftKey]Draft
		problems []string
	}
	savedMsg struct {
		unsaved []DraftKey
		err     error
	}
	manifest struct {
		Version   int              `json:"version"`
		Endpoints []string         `json:"endpoints"`
		Windows   []manifestWindow `json:"windows"`
		Drafts    []manifestDraft  `json:"drafts"`
	}
	manifestWindow struct {
		ID      string         `json:"id"`
		Threads []manifestView `json:"threads"`
	}
	manifestView struct {
		Endpoint string `json:"endpoint"`
		Thread   string `json:"thread"`
	}
	manifestDraft struct {
		Endpoint string `json:"endpoint"`
		Window   string `json:"window"`
		Thread   string `json:"thread"`
		File     string `json:"file"`
	}
	savedDraft struct {
		Endpoint    string            `json:"endpoint"`
		Window      string            `json:"window"`
		Thread      string            `json:"thread"`
		Text        string            `json:"text"`
		Attachments []savedAttachment `json:"attachments"`
	}
	savedAttachment struct {
		ID   string `json:"id"`
		Kind uint8  `json:"kind"`
		Name string `json:"name"`
		Text string `json:"text,omitzero"`
		URL  string `json:"url,omitzero"`
	}
)

var errRestoreFormat = errors.New("the saved windows use an unknown format")

func (msg restoredMsg) apply(m *machine) {
	for _, w := range msg.windows {
		for _, v := range w.Threads {
			m.watch(m.endpoint(client.EndpointID(v.Endpoint)), WindowID(w.ID), ThreadKey{Endpoint: client.EndpointID(v.Endpoint), Thread: v.Thread})
		}
	}
	maps.Copy(m.drafts, msg.drafts)
	for _, problem := range msg.problems {
		m.notify(Notice{Text: problem})
	}
}

func (msg savedMsg) apply(m *machine) {
	m.unsaved = map[DraftKey]bool{}
	for _, key := range msg.unsaved {
		m.unsaved[key] = true
	}
	if msg.err != nil {
		m.notify(Notice{Text: fmt.Sprintf("Could not save open windows and drafts: %v", msg.err)})
	}
}

func (m *machine) restoreImage() restoreImage {
	windows := make(map[WindowID][]ThreadKey, len(m.windows))
	for id, keys := range m.windows {
		windows[id] = slices.Clone(keys)
	}
	drafts := maps.Clone(m.drafts)
	maps.DeleteFunc(drafts, func(_ DraftKey, d Draft) bool { return d.empty() })
	return restoreImage{windows: windows, drafts: drafts}
}

func draftFile(key DraftKey) string {
	sum := sha256.Sum256([]byte(string(key.Endpoint) + "\x00" + string(key.Window) + "\x00" + key.Thread))
	return hex.EncodeToString(sum[:16]) + draftSuffix
}

func loadRestore(dir string) (restoredMsg, map[string]draftDigest) {
	written := map[string]draftDigest{}
	msg := restoredMsg{drafts: map[DraftKey]Draft{}}
	if dir == "" {
		return msg, written
	}
	saved, err := readManifest(dir)
	if errors.Is(err, fs.ErrNotExist) {
		return msg, written
	}
	if err != nil {
		msg.problems = append(msg.problems, fmt.Sprintf("Could not restore the saved windows: %v", err))
		return msg, written
	}
	msg.windows = saved.Windows
	budget := draftBudget
	for _, ref := range saved.Drafts {
		d, data, err := readDraft(dir, ref)
		if err == nil && len(data) > budget {
			err = fmt.Errorf("the saved drafts exceed %d MiB", draftBudget>>20)
		}
		if err != nil {
			msg.problems = append(msg.problems, fmt.Sprintf("Could not restore a saved draft: %v", err))
			continue
		}
		budget -= len(data)
		msg.drafts[ref.key()] = d
		written[ref.File] = sha256.Sum256(data)
	}
	return msg, written
}

func readManifest(dir string) (manifest, error) {
	data, err := readLimited(filepath.Join(dir, manifestName), manifestLimit)
	if err != nil {
		return manifest{}, err
	}
	var saved manifest
	if err := json.Unmarshal(data, &saved); err != nil {
		return manifest{}, fmt.Errorf("%w: %w", errRestoreFormat, err)
	}
	views := 0
	for _, w := range saved.Windows {
		views += len(w.Threads)
	}
	switch {
	case saved.Version != restoreVersion:
		return manifest{}, fmt.Errorf("%w: version %d", errRestoreFormat, saved.Version)
	case len(saved.Endpoints) > endpointRefLimit, views > threadRefLimit, len(saved.Drafts) > draftRefLimit:
		return manifest{}, fmt.Errorf("%w: more references than the limits allow", errRestoreFormat)
	}
	return saved, nil
}

func (ref manifestDraft) key() DraftKey {
	return DraftKey{Endpoint: client.EndpointID(ref.Endpoint), Window: WindowID(ref.Window), Thread: ref.Thread}
}

func readDraft(dir string, ref manifestDraft) (Draft, []byte, error) {
	if ref.File != draftFile(ref.key()) {
		return Draft{}, nil, fmt.Errorf("%w: draft file %q", errRestoreFormat, ref.File)
	}
	data, err := readLimited(filepath.Join(dir, ref.File), draftFileLimit)
	if err != nil {
		return Draft{}, nil, err
	}
	var saved savedDraft
	if err := json.Unmarshal(data, &saved); err != nil {
		return Draft{}, nil, fmt.Errorf("%w: %w", errRestoreFormat, err)
	}
	if saved.Endpoint != ref.Endpoint || saved.Window != ref.Window || saved.Thread != ref.Thread {
		return Draft{}, nil, fmt.Errorf("%w: draft %s belongs to another thread", errRestoreFormat, ref.File)
	}
	d := Draft{Text: saved.Text}
	for _, a := range saved.Attachments {
		d.Attachments = append(d.Attachments, Attachment{ID: a.ID, Kind: AttachmentKind(a.Kind), Name: a.Name, Text: a.Text, URL: a.URL})
	}
	return d, data, nil
}

func readLimited(path string, limit int64) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, limit+1))
	if err == nil && int64(len(data)) > limit {
		err = fmt.Errorf("%s exceeds %d bytes", filepath.Base(path), limit)
	}
	return data, err
}

func writeRestore(dir string, img restoreImage, written map[string]draftDigest) ([]DraftKey, error) {
	if dir == "" {
		return nil, nil
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, err
	}
	saved := manifest{Version: restoreVersion, Windows: manifestWindows(img.windows)}
	unsaved, err := writeDrafts(dir, img.drafts, written, &saved)
	if err != nil {
		return unsaved, err
	}
	saved.Endpoints = manifestEndpoints(saved)
	data, err := json.Marshal(saved)
	if err != nil {
		return unsaved, err
	}
	if err := writeAtomic(dir, manifestName, data); err != nil {
		return unsaved, err
	}
	return unsaved, removeOrphans(dir, saved, written)
}

func manifestWindows(windows map[WindowID][]ThreadKey) []manifestWindow {
	out := []manifestWindow{}
	views := 0
	for _, id := range slices.Sorted(maps.Keys(windows)) {
		w := manifestWindow{ID: string(id)}
		for _, key := range windows[id] {
			if views == threadRefLimit {
				break
			}
			w.Threads = append(w.Threads, manifestView{Endpoint: string(key.Endpoint), Thread: key.Thread})
			views++
		}
		out = append(out, w)
	}
	return out
}

func manifestEndpoints(saved manifest) []string {
	var ids []string
	for _, w := range saved.Windows {
		for _, v := range w.Threads {
			ids = append(ids, v.Endpoint)
		}
	}
	for _, d := range saved.Drafts {
		ids = append(ids, d.Endpoint)
	}
	slices.Sort(ids)
	return slices.Compact(ids)
}

func compareDraftKeys(a, b DraftKey) int {
	return cmp.Or(cmp.Compare(a.Endpoint, b.Endpoint), cmp.Compare(a.Window, b.Window), cmp.Compare(a.Thread, b.Thread))
}

func writeDrafts(dir string, drafts map[DraftKey]Draft, written map[string]draftDigest, saved *manifest) ([]DraftKey, error) {
	var unsaved []DraftKey
	budget := draftBudget
	for _, key := range slices.SortedFunc(maps.Keys(drafts), compareDraftKeys) {
		data, err := json.Marshal(savedDraftOf(key, drafts[key]))
		if err != nil {
			return unsaved, err
		}
		if len(data) > draftFileLimit || len(data) > budget || len(saved.Drafts) == draftRefLimit {
			unsaved = append(unsaved, key)
			continue
		}
		name := draftFile(key)
		if sum := sha256.Sum256(data); written[name] != sum {
			if err := writeAtomic(dir, name, data); err != nil {
				return unsaved, err
			}
			written[name] = sum
		}
		budget -= len(data)
		saved.Drafts = append(saved.Drafts, manifestDraft{Endpoint: string(key.Endpoint), Window: string(key.Window), Thread: key.Thread, File: name})
	}
	return unsaved, nil
}

func savedDraftOf(key DraftKey, d Draft) savedDraft {
	saved := savedDraft{Endpoint: string(key.Endpoint), Window: string(key.Window), Thread: key.Thread, Text: d.Text, Attachments: []savedAttachment{}}
	for _, a := range d.Attachments {
		saved.Attachments = append(saved.Attachments, savedAttachment{ID: a.ID, Kind: uint8(a.Kind), Name: a.Name, Text: a.Text, URL: a.URL})
	}
	return saved
}

func writeAtomic(dir, name string, data []byte) error {
	f, err := os.CreateTemp(dir, "."+name+"-*")
	if err != nil {
		return err
	}
	_, err = f.Write(data)
	if err == nil {
		err = f.Sync()
	}
	if closeErr := f.Close(); err == nil {
		err = closeErr
	}
	if err == nil {
		err = os.Rename(f.Name(), filepath.Join(dir, name))
	}
	if err != nil {
		return errors.Join(err, os.Remove(f.Name()))
	}
	return nil
}

func removeOrphans(dir string, saved manifest, written map[string]draftDigest) error {
	kept := map[string]bool{}
	for _, d := range saved.Drafts {
		kept[d.File] = true
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return err
	}
	var errs []error
	for _, entry := range entries {
		name := entry.Name()
		if strings.HasSuffix(name, draftSuffix) && !kept[name] {
			delete(written, name)
			errs = append(errs, os.Remove(filepath.Join(dir, name)))
		}
	}
	return errors.Join(errs...)
}
