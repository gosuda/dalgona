package agent

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"maps"
	"os"
	"regexp"
	"slices"
	"strings"
	"testing"
)

const (
	corpusCasesPath   = "testdata/editreview/cases.jsonl"
	corpusRatchetPath = "testdata/editreview/ratchet.json"
	corpusTotal       = 472
	corpusJudge       = "decisions:typesafe/jev-1.14"
	corpusThreshold   = 0.9
	corpusTool        = "apply_patch"
	groupSoundEdit    = "sound-edit"
	groupSoundTurn    = "sound-turn"
	groupMultipart    = "multipart"
	groupMoved        = "moved"
	corpusSplitDev    = "dev"
	corpusSplitHeld   = "held_out"
)

var (
	corpusDefectIDs = []string{"broken_syntax", "lost_code", "wrong_logic", "placeholder", "off_task", "needless_layer", "narration", "unguarded_state", "wasted_work", "unsafe_input", "unclear_contract", "off_token", "layout_motion", "incomplete"}
	corpusKindQuota = map[string]int{"code": 60, "interface": 24, "config": 12, "text": 12}
	movedSignature  = regexp.MustCompile(`^([+-])func ([A-Za-z_]\w*)\(`)
)

type corpusFile struct {
	Path string `json:"path"`
	Kind string `json:"kind"`
	Pre  string `json:"pre"`
	Post string `json:"post"`
}

type corpusCase struct {
	ID     string       `json:"id"`
	Group  string       `json:"group"`
	Unit   string       `json:"unit"`
	Split  string       `json:"split"`
	Source string       `json:"source"`
	Task   string       `json:"task"`
	Files  []corpusFile `json:"files"`
	Flags  []string     `json:"flags"`
}

type ratchetQuestion struct {
	Precision *float64 `json:"precision"`
	Recall    *float64 `json:"recall"`
	TrueFlags int      `json:"true_flags"`
}

type ratchetMeasured struct {
	SoundFlagRate float64                    `json:"sound_flag_rate"`
	Errors        int                        `json:"errors"`
	Questions     map[string]ratchetQuestion `json:"questions"`
}

type ratchetFile struct {
	CorpusSHA256 string           `json:"corpus_sha256"`
	Judge        string           `json:"judge"`
	Threshold    float64          `json:"threshold"`
	Measured     *ratchetMeasured `json:"measured"`
}

type movedRef struct {
	name           string
	removed, added int
}

type funcHit struct {
	file string
	part int
}

var corpusDefectQuota = map[string]int{"broken_syntax": 30, "lost_code": 30, "wrong_logic": 30, "placeholder": 30, "off_task": 30, "needless_layer": 7, "narration": 30, "unguarded_state": 2, "wasted_work": 23, "unsafe_input": 30, "unclear_contract": 19, "off_token": 0, "layout_motion": 2, "incomplete": 30}

func corpusGroupSizes() map[string]int {
	sizes := map[string]int{groupSoundEdit: 108, groupSoundTurn: 30, groupMultipart: 40, groupMoved: 1}
	for _, id := range corpusDefectIDs {
		sizes[id] = corpusDefectQuota[id]
	}
	return sizes
}

func defectQuotaTotal() int {
	total := 0
	for _, id := range corpusDefectIDs {
		total += corpusDefectQuota[id]
	}
	return total
}

func hashHex(s string) string {
	sum := sha256.Sum256([]byte(s))
	return hex.EncodeToString(sum[:])
}

func corpusChange(f corpusFile) string {
	switch {
	case f.Pre == "":
		return "add"
	case f.Post == "":
		return "delete"
	}
	return "update"
}

func corpusReviewFiles(c corpusCase) []reviewFile {
	out := make([]reviewFile, 0, len(c.Files))
	for _, f := range c.Files {
		out = append(out, classifyReviewFile(f.Path, corpusChange(f), []byte(f.Pre), []byte(f.Post)))
	}
	return out
}

func caseParts(t testing.TB, c corpusCase) []judgeInput {
	t.Helper()
	parts, _, err := buildReview(c.Task, corpusTool, corpusReviewFiles(c))
	if err != nil {
		t.Fatalf("case %s: build review: %v", c.ID, err)
	}
	return parts
}

func readCorpus(t testing.TB) []corpusCase {
	t.Helper()
	raw, err := os.ReadFile(corpusCasesPath)
	if err != nil {
		t.Fatalf("read corpus: %v", err)
	}
	var cases []corpusCase
	for line := range bytes.SplitSeq(raw, []byte("\n")) {
		if len(line) == 0 {
			continue
		}
		var c corpusCase
		if err := json.Unmarshal(line, &c); err != nil {
			t.Fatalf("decode corpus line: %v", err)
		}
		cases = append(cases, c)
	}
	return cases
}

func readRatchet(t testing.TB) ratchetFile {
	t.Helper()
	raw, err := os.ReadFile(corpusRatchetPath)
	if err != nil {
		t.Fatalf("read ratchet: %v", err)
	}
	var r ratchetFile
	if err := json.Unmarshal(raw, &r); err != nil {
		t.Fatalf("decode ratchet: %v", err)
	}
	return r
}

func expectedSplits(ids []string) map[string]string {
	sorted := slices.Clone(ids)
	slices.SortFunc(sorted, func(a, b string) int { return strings.Compare(hashHex(a), hashHex(b)) })
	out := make(map[string]string, len(ids))
	for i, id := range sorted {
		out[id] = corpusSplitDev
		if i%2 == 1 {
			out[id] = corpusSplitHeld
		}
	}
	return out
}

func checkCaseShape(t *testing.T, c corpusCase, split string) {
	t.Helper()
	if c.Split != split {
		t.Errorf("case %s: split %s, want %s", c.ID, c.Split, split)
	}
	if c.Source == "" || c.Task == "" || len(c.Files) == 0 {
		t.Errorf("case %s: source, task or files empty", c.ID)
	}
	if c.Unit == "turn" {
		if _, err := maskedParts(func(s string) (string, error) { return s, nil }, c.Task, corpusReviewFiles(c), c.Unit); err != nil {
			t.Errorf("case %s: %v", c.ID, err)
		}
	}
	checkCaseGroup(t, c)
}

func checkCaseGroup(t *testing.T, c corpusCase) {
	t.Helper()
	if c.Group == groupSoundEdit && (len(c.Files) != 1 || c.Unit != "edit") {
		t.Errorf("case %s: sound edit must hold one file and unit edit", c.ID)
	}
	if want := corpusUnit(c.Group); c.Unit != want {
		t.Errorf("case %s: unit %s, want %s", c.ID, c.Unit, want)
	}
	if !slices.Equal(c.Flags, corpusFlags(c.Group)) {
		t.Errorf("case %s: flags %v, want %v", c.ID, c.Flags, corpusFlags(c.Group))
	}
}

func corpusUnit(group string) string {
	if group == groupSoundTurn || group == "incomplete" {
		return "turn"
	}
	return "edit"
}

func corpusFlags(group string) []string {
	if slices.Contains(corpusDefectIDs, group) {
		return []string{group}
	}
	return []string{}
}

func checkKindQuota(t *testing.T, cases []corpusCase) {
	t.Helper()
	got := map[string]int{}
	for _, c := range cases {
		got[reviewKind(c.Files[0].Path)]++
	}
	for kind, want := range corpusKindQuota {
		if got[kind] != want {
			t.Errorf("sound edit kind %s: %d cases, want %d", kind, got[kind], want)
		}
	}
}

func groupCases(cases []corpusCase) map[string][]corpusCase {
	out := map[string][]corpusCase{}
	for _, c := range cases {
		out[c.Group] = append(out[c.Group], c)
	}
	return out
}

func checkCorpusCount(t *testing.T, cases []corpusCase) {
	t.Helper()
	if len(cases) != corpusTotal {
		t.Fatalf("corpus holds %d cases, want %d", len(cases), corpusTotal)
	}
	seen := map[string]bool{}
	for _, c := range cases {
		if seen[c.ID] {
			t.Fatalf("duplicate case id %s", c.ID)
		}
		seen[c.ID] = true
	}
}

func checkRatchetSHA(t *testing.T) {
	t.Helper()
	raw, err := os.ReadFile(corpusCasesPath)
	if err != nil {
		t.Fatalf("read corpus: %v", err)
	}
	sum := sha256.Sum256(raw)
	if got := readRatchet(t).CorpusSHA256; got != hex.EncodeToString(sum[:]) {
		t.Fatalf("ratchet corpus_sha256 %s does not match cases.jsonl", got)
	}
}

func TestEditReviewCorpusSplit(t *testing.T) {
	cases := readCorpus(t)
	checkCorpusCount(t, cases)
	byGroup := groupCases(cases)
	for group, want := range corpusGroupSizes() {
		members := byGroup[group]
		if len(members) != want {
			t.Fatalf("group %s holds %d cases, want %d", group, len(members), want)
		}
		ids := make([]string, 0, len(members))
		for _, c := range members {
			ids = append(ids, c.ID)
		}
		splits := expectedSplits(ids)
		for _, c := range members {
			checkCaseShape(t, c, splits[c.ID])
		}
	}
	checkKindQuota(t, byGroup[groupSoundEdit])
	checkRatchetSHA(t)
}

func TestEditReviewCorpusMultipart(t *testing.T) {
	for _, c := range readCorpus(t) {
		switch c.Group {
		case groupMultipart:
			if n := len(caseParts(t, c)); n < 2 {
				t.Errorf("case %s: %d parts, want at least 2", c.ID, n)
			}
		case groupMoved:
			ref, ok := movedFunction(caseParts(t, c))
			if !ok {
				t.Errorf("case %s: no function removed from one file and added to another", c.ID)
				continue
			}
			if ref.removed == ref.added {
				t.Errorf("case %s: removal and addition of %s land in part %d", c.ID, ref.name, ref.removed+1)
			}
		}
	}
}

func topLevelFunc(line string) (sign byte, name string, ok bool) {
	sub := movedSignature.FindStringSubmatch(line)
	if sub == nil {
		return 0, "", false
	}
	return sub[1][0], sub[2], true
}

func funcHitsOf(parts []judgeInput) (removed, added map[string][]funcHit, ok bool) {
	removed, added = map[string][]funcHit{}, map[string][]funcHit{}
	for i, part := range parts {
		var state judgeState
		if err := json.Unmarshal(part.state, &state); err != nil {
			return nil, nil, false
		}
		for _, f := range state.Files {
			addFuncHits(removed, added, f, i)
		}
	}
	return removed, added, true
}

func addFuncHits(removed, added map[string][]funcHit, f judgeFile, part int) {
	for line := range strings.SplitSeq(f.Diff, "\n") {
		sign, name, ok := topLevelFunc(line)
		if !ok {
			continue
		}
		hit := funcHit{file: f.Path, part: part}
		if sign == '-' {
			removed[name] = append(removed[name], hit)
		} else {
			added[name] = append(added[name], hit)
		}
	}
}

func movedFunction(parts []judgeInput) (movedRef, bool) {
	removed, added, ok := funcHitsOf(parts)
	if !ok {
		return movedRef{}, false
	}
	var first movedRef
	seen := false
	for _, name := range slices.Sorted(maps.Keys(removed)) {
		for _, ref := range movedPairs(name, removed[name], added[name]) {
			if ref.removed != ref.added {
				return ref, true
			}
			if !seen {
				first, seen = ref, true
			}
		}
	}
	return first, seen
}

func ratchetSpeakers(r ratchetFile) []string {
	if r.Measured == nil {
		return nil
	}
	var out []string
	for _, q := range reviewQuestions {
		row, ok := r.Measured.Questions[q.id]
		if q.id == soundID || !ok || row.Precision == nil {
			continue
		}
		if *row.Precision >= r.Threshold && row.TrueFlags >= 5 {
			out = append(out, q.id)
		}
	}
	return out
}

func movedPairs(name string, removed, added []funcHit) []movedRef {
	var out []movedRef
	for _, r := range removed {
		for _, a := range added {
			if r.file != a.file {
				out = append(out, movedRef{name: name, removed: r.part, added: a.part})
			}
		}
	}
	return out
}

func TestBuiltInSpeakersMatchRatchet(t *testing.T) {
	r := readRatchet(t)
	if derived := ratchetSpeakers(r); !slices.Equal(derived, reviewBuiltInSpeakers) {
		t.Fatalf("ratchet derives speakers %v, reviewBuiltInSpeakers holds %v", derived, reviewBuiltInSpeakers)
	}
	for _, id := range reviewBuiltInSpeakers {
		if r.Measured == nil {
			t.Fatalf("speaker %s has no ratchet row: measured is null", id)
		}
		if _, ok := r.Measured.Questions[id]; !ok {
			t.Fatalf("speaker %s has no ratchet row", id)
		}
	}
}

func marshalRatchet(r ratchetFile) ([]byte, error) {
	out, err := json.Marshal(r, jsontext.WithIndent("  "))
	if err != nil {
		return nil, fmt.Errorf("encode ratchet: %w", err)
	}
	return append(out, '\n'), nil
}

func TestSpeakersComeFromQuestionTable(t *testing.T) {
	table := slices.Clone(reviewQuestions)
	for i := range table {
		if table[i].id == "lost_code" {
			table[i].speaker = true
		}
	}
	if got := speakersOf(table); !slices.Equal(got, []string{"lost_code"}) {
		t.Fatalf("speakersOf reads the speaker flag of lost_code as %v, want [lost_code]", got)
	}
	if want := speakersOf(reviewQuestions); !slices.Equal(reviewBuiltInSpeakers, want) {
		t.Fatalf("reviewBuiltInSpeakers holds %v, the question table marks %v", reviewBuiltInSpeakers, want)
	}
}
