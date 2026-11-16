package snapshot

import (
	"errors"
	"fmt"
	"math/rand/v2"
	"strings"
	"sync"
	"testing"
)

const (
	evaluatorConsumer = "evaluator"
	subagentConsumer  = "subagent"
	evidenceBody      = "one\ntwo\nthree\n"
	evidenceLines     = "a\nb\nc\nd\ne\nf\n"
)

func evidenceConsumers() []string {
	return []string{ConsumerModel, evaluatorConsumer, subagentConsumer}
}

func evidenceSentinels() []error {
	return []error{ErrUnknownReference, ErrExpiredReference, ErrBindingMismatch, ErrPathChanged, ErrStaleReference, ErrNoBoundary}
}

func requireVerdict(t *testing.T, name string, got, want error) {
	t.Helper()
	if (got == nil) != (want == nil) {
		t.Fatalf("%s: error %v, want %v", name, got, want)
	}
	for _, sentinel := range evidenceSentinels() {
		if errors.Is(got, sentinel) != errors.Is(want, sentinel) {
			t.Fatalf("%s: error %v, want only %v", name, got, want)
		}
	}
}

func evidenceRef(store *Store, shown ...Range) string {
	return store.Put("f.txt", []byte(evidenceBody), shown)
}

func modelAt(cutoff uint64) Evidence {
	return Evidence{Consumer: ConsumerModel, Cutoff: cutoff}
}

func TestEligibleFollowsDeliveryAndCutoff(t *testing.T) {
	store := New()
	cases := []struct {
		name     string
		delivery uint64
		cutoff   uint64
		want     error
	}{
		{"never delivered", 0, 100, ErrUnknownReference},
		{"preview lands after the request is frozen", 8, 7, ErrUnknownReference},
		{"next request after the preview", 8, 8, nil},
		{"much later request", 8, 20, nil},
		{"first journal record", 1, 1, nil},
		{"one record past the cutoff", 2, 1, ErrUnknownReference},
	}
	for _, item := range cases {
		ref := evidenceRef(store, Range{Start: 1, End: 3})
		if item.delivery != 0 {
			store.Bind(ref, ConsumerModel, item.delivery)
		}
		requireVerdict(t, item.name, store.Eligible(ref, modelAt(item.cutoff)), item.want)
	}
}

func TestEligibleRejectsAnotherConsumer(t *testing.T) {
	store := New()
	ref := evidenceRef(store, Range{Start: 1, End: 3})
	store.Bind(ref, ConsumerModel, 4)
	foreign := Evidence{Consumer: evaluatorConsumer, Cutoff: 9}
	err := store.Eligible(ref, foreign)
	requireVerdict(t, "evaluator", err, ErrBindingMismatch)
	for _, leak := range []string{ref, "f.txt", ConsumerModel, evaluatorConsumer} {
		if strings.Contains(err.Error(), leak) {
			t.Fatalf("binding error leaks %q: %q", leak, err.Error())
		}
	}
	covered, observedErr := store.Observed(ref, foreign, 1, 3)
	if covered {
		t.Fatal("another consumer observed the lines")
	}
	requireVerdict(t, "evaluator observed", observedErr, ErrBindingMismatch)
	requireVerdict(t, "owner", store.Eligible(ref, modelAt(9)), nil)
}

func TestBindKeepsFirstDeliveryAndConsumer(t *testing.T) {
	store := New()
	ref := evidenceRef(store, Range{Start: 1, End: 3})
	store.Bind(ref, ConsumerModel, 6)
	store.Bind(ref, ConsumerModel, 9)
	store.Bind(ref, ConsumerModel, 3)
	store.Bind(ref, evaluatorConsumer, 2)
	entry, err := store.Get(ref)
	if err != nil {
		t.Fatal(err)
	}
	if entry.Consumer != ConsumerModel || entry.Delivery != 6 {
		t.Fatalf("binding moved: consumer %q delivery %d", entry.Consumer, entry.Delivery)
	}
	requireVerdict(t, "before the first delivery", store.Eligible(ref, modelAt(5)), ErrUnknownReference)
	requireVerdict(t, "at the first delivery", store.Eligible(ref, modelAt(6)), nil)
	requireVerdict(t, "evaluator", store.Eligible(ref, Evidence{Consumer: evaluatorConsumer, Cutoff: 9}), ErrBindingMismatch)
}

func TestBindIgnoresInvalidDelivery(t *testing.T) {
	store := New()
	ref := evidenceRef(store, Range{Start: 1, End: 3})
	store.Bind(ref, "", 5)
	store.Bind(ref, ConsumerModel, 0)
	store.Bind("not a reference", ConsumerModel, 5)
	entry, err := store.Get(ref)
	if err != nil {
		t.Fatal(err)
	}
	if entry.Consumer != "" || entry.Delivery != 0 {
		t.Fatalf("invalid delivery bound the reference: consumer %q delivery %d", entry.Consumer, entry.Delivery)
	}
	store.Bind(ref, ConsumerModel, 5)
	requireVerdict(t, "valid delivery", store.Eligible(ref, modelAt(5)), nil)
	var missing *Store
	missing.Bind(ref, ConsumerModel, 5)
	requireVerdict(t, "nil store", missing.Eligible(ref, modelAt(5)), ErrUnknownReference)
}

func TestEligibleClassifiesAbsentReferences(t *testing.T) {
	store := New()
	evicted := evidenceRef(store)
	for range maxEntries {
		evidenceRef(store)
	}
	store.Bind(evicted, ConsumerModel, 3)
	cases := []struct {
		name string
		ref  string
		want error
	}{
		{"evicted", evicted, ErrExpiredReference},
		{"another store", New().Put("f.txt", []byte(evidenceBody), nil), ErrUnknownReference},
		{"malformed", "not a reference", ErrUnknownReference},
	}
	for _, item := range cases {
		requireVerdict(t, item.name, store.Eligible(item.ref, modelAt(9)), item.want)
		covered, err := store.Observed(item.ref, modelAt(9), 1, 1)
		if covered {
			t.Fatalf("%s: observed lines", item.name)
		}
		requireVerdict(t, item.name+" observed", err, item.want)
	}
	if _, err := store.Get(evicted); !errors.Is(err, ErrExpiredReference) {
		t.Fatalf("bind resurrected an evicted reference: %v", err)
	}
}

func TestEligibleFailsClosedWithoutBoundary(t *testing.T) {
	store := New()
	ref := evidenceRef(store, Range{Start: 1, End: 3})
	store.Bind(ref, ConsumerModel, 1)
	stranger := New().Put("f.txt", []byte(evidenceBody), nil)
	cases := []struct {
		name     string
		evidence Evidence
	}{
		{"zero value", Evidence{}},
		{"no cutoff", Evidence{Consumer: ConsumerModel}},
		{"no consumer", Evidence{Cutoff: 9}},
	}
	for _, item := range cases {
		if item.evidence.Bounded() {
			t.Fatalf("%s: bounded", item.name)
		}
		for _, target := range []string{ref, stranger} {
			requireVerdict(t, item.name, store.Eligible(target, item.evidence), ErrNoBoundary)
			covered, err := store.Observed(target, item.evidence, 1, 3)
			if covered {
				t.Fatalf("%s: observed lines without a boundary", item.name)
			}
			requireVerdict(t, item.name+" observed", err, ErrNoBoundary)
		}
	}
	if !modelAt(9).Bounded() {
		t.Fatal("complete evidence is unbounded")
	}
	requireVerdict(t, "bounded control", store.Eligible(ref, modelAt(9)), nil)
}

func TestEligibleIgnoresCaptureWithoutDelivery(t *testing.T) {
	store := New()
	entry, ok := store.Capture("f.txt", []byte(evidenceBody), []Range{{Start: 1, End: 3}})
	if !ok {
		t.Fatal("capture failed")
	}
	if !store.Covers(entry.Ref, 1, 3) {
		t.Fatal("precondition: the capture does not hold the lines")
	}
	if entry.Consumer != "" || entry.Delivery != 0 {
		t.Fatalf("capture arrived bound: consumer %q delivery %d", entry.Consumer, entry.Delivery)
	}
	requireVerdict(t, "capture", store.Eligible(entry.Ref, modelAt(99)), ErrUnknownReference)
	covered, err := store.Observed(entry.Ref, modelAt(99), 1, 3)
	if covered {
		t.Fatal("a capture without delivery counts as observed")
	}
	requireVerdict(t, "capture observed", err, ErrUnknownReference)
}

func TestEvidenceObservedLines(t *testing.T) {
	store := New()
	ref := evidenceRef(store, Range{Start: 2, End: 2})
	store.Bind(ref, ConsumerModel, 3)
	cases := []struct {
		name        string
		evidence    Evidence
		first, last int
		covered     bool
		want        error
	}{
		{"delivered complete line", modelAt(3), 2, 2, true, nil},
		{"line before the shown range", modelAt(3), 1, 1, false, nil},
		{"range wider than the shown range", modelAt(3), 2, 3, false, nil},
		{"line zero", modelAt(3), 0, 2, false, nil},
		{"reversed range", modelAt(3), 3, 2, false, nil},
		{"request frozen before the delivery", modelAt(2), 2, 2, false, ErrUnknownReference},
		{"later request", modelAt(40), 2, 2, true, nil},
	}
	for _, item := range cases {
		covered, err := store.Observed(ref, item.evidence, item.first, item.last)
		requireVerdict(t, item.name, err, item.want)
		if covered != item.covered {
			t.Fatalf("%s: covered %v, want %v", item.name, covered, item.covered)
		}
	}
}

func TestBindRestoredReferenceNeedsJournalDelivery(t *testing.T) {
	data := []byte(evidenceBody)
	store := New()
	ref := store.Put("f.txt", data, []Range{{Start: 2, End: 2}})
	store.Bind(ref, ConsumerModel, 4)
	entry, err := store.Get(ref)
	if err != nil {
		t.Fatal(err)
	}
	if entry.Delivery != 4 {
		t.Fatalf("precondition: delivery %d", entry.Delivery)
	}
	bare := entry
	bare.Delivery, bare.Consumer = 0, ""
	if string(entry.Meta()) != string(bare.Meta()) {
		t.Fatalf("metadata carries delivery state: %s", entry.Meta())
	}
	rebuilt := New()
	if restoreErr := rebuilt.Restore(entry.Meta(), data); restoreErr != nil {
		t.Fatal(restoreErr)
	}
	requireVerdict(t, "restored", rebuilt.Eligible(ref, modelAt(99)), ErrUnknownReference)
	rebuilt.Bind(ref, ConsumerModel, 4)
	requireVerdict(t, "restored and bound", rebuilt.Eligible(ref, modelAt(4)), nil)
	covered, err := rebuilt.Observed(ref, modelAt(4), 2, 2)
	requireVerdict(t, "restored and bound observed", err, nil)
	if !covered {
		t.Fatal("restored shown range lost")
	}
}

func TestEvidenceParallelCallsShareOneCutoff(t *testing.T) {
	store := New()
	early := evidenceRef(store, Range{Start: 1, End: 3})
	preview := evidenceRef(store, Range{Start: 1, End: 3})
	store.Bind(early, ConsumerModel, 5)
	frozen := modelAt(7)
	start := make(chan struct{})
	var group sync.WaitGroup
	group.Go(func() {
		<-start
		store.Bind(preview, ConsumerModel, 8)
	})
	for range 8 {
		group.Go(func() {
			<-start
			for range 200 {
				if err := store.Eligible(preview, frozen); !errors.Is(err, ErrUnknownReference) {
					t.Errorf("preview delivered after the cutoff gave %v", err)
					return
				}
				if err := store.Eligible(early, frozen); err != nil {
					t.Errorf("delivery before the cutoff gave %v", err)
					return
				}
			}
		})
	}
	close(start)
	group.Wait()
	requireVerdict(t, "next request", store.Eligible(preview, modelAt(8)), nil)
}

type delivery struct {
	consumer string
	seq      uint64
}

type evidenceModel struct {
	history map[string][]delivery
	lines   map[string]map[int]bool
}

func newEvidenceModel() *evidenceModel {
	return &evidenceModel{history: make(map[string][]delivery), lines: make(map[string]map[int]bool)}
}

func (m *evidenceModel) bind(ref, consumer string, seq uint64) {
	m.history[ref] = append(m.history[ref], delivery{consumer: consumer, seq: seq})
}

func (m *evidenceModel) eligible(ref string, evidence Evidence) error {
	if evidence.Consumer == "" || evidence.Cutoff == 0 {
		return ErrNoBoundary
	}
	history := m.history[ref]
	switch {
	case len(history) > 0 && history[0].consumer != evidence.Consumer:
		return ErrBindingMismatch
	case len(history) == 0 || history[0].seq > evidence.Cutoff:
		return ErrUnknownReference
	}
	return nil
}

func (m *evidenceModel) observed(ref string, evidence Evidence, first, last int) (bool, error) {
	if err := m.eligible(ref, evidence); err != nil {
		return false, err
	}
	for line := first; line <= last; line++ {
		if !m.lines[ref][line] {
			return false, nil
		}
	}
	return first >= 1 && last >= first, nil
}

func lineSet(ranges []Range) map[int]bool {
	set := make(map[int]bool)
	for _, item := range ranges {
		for line := item.Start; line <= item.End; line++ {
			set[line] = true
		}
	}
	return set
}

func randomRanges(random *rand.Rand) []Range {
	ranges := make([]Range, random.IntN(4))
	for i := range ranges {
		start := 1 + random.IntN(6)
		ranges[i] = Range{Start: start, End: start + random.IntN(7-start)}
	}
	return ranges
}

type evidenceTally struct {
	covered, uncovered, mismatch, unknown int
}

func (c *evidenceTally) record(err error, covered bool) {
	switch {
	case errors.Is(err, ErrBindingMismatch):
		c.mismatch++
	case errors.Is(err, ErrUnknownReference):
		c.unknown++
	case err != nil:
	case covered:
		c.covered++
	default:
		c.uncovered++
	}
}

type evidenceRun struct {
	store  *Store
	model  *evidenceModel
	random *rand.Rand
	refs   []string
	tally  *evidenceTally
}

func newEvidenceRun(seed uint64, tally *evidenceTally) *evidenceRun {
	random := rand.New(rand.NewPCG(seed, seed+1))
	run := &evidenceRun{store: New(), model: newEvidenceModel(), random: random, tally: tally}
	for range 8 {
		ranges := randomRanges(random)
		ref := run.store.Put("f.txt", []byte(evidenceLines), ranges)
		run.refs = append(run.refs, ref)
		run.model.lines[ref] = lineSet(ranges)
	}
	return run
}

func (r *evidenceRun) pick() (string, string) {
	consumers := evidenceConsumers()
	return r.refs[r.random.IntN(len(r.refs))], consumers[r.random.IntN(len(consumers))]
}

func (r *evidenceRun) bind() {
	ref, consumer := r.pick()
	seq := 1 + r.random.Uint64N(30)
	r.store.Bind(ref, consumer, seq)
	r.model.bind(ref, consumer, seq)
}

func (r *evidenceRun) probe(t *testing.T, label string) {
	t.Helper()
	ref, consumer := r.pick()
	evidence := Evidence{Consumer: consumer, Cutoff: r.random.Uint64N(31)}
	first := 1 + r.random.IntN(6)
	last := first + r.random.IntN(7-first)
	requireVerdict(t, label, r.store.Eligible(ref, evidence), r.model.eligible(ref, evidence))
	covered, err := r.store.Observed(ref, evidence, first, last)
	want, wantErr := r.model.observed(ref, evidence, first, last)
	requireVerdict(t, label+" observed", err, wantErr)
	if covered != want {
		t.Fatalf("%s: covered %v, want %v", label, covered, want)
	}
	r.tally.record(wantErr, want)
}

func TestEvidenceCutoffProperty(t *testing.T) {
	var tally evidenceTally
	for seed := range uint64(200) {
		run := newEvidenceRun(seed, &tally)
		for step := range 60 {
			if run.random.IntN(2) == 0 {
				run.bind()
				continue
			}
			run.probe(t, fmt.Sprintf("seed %d step %d", seed, step))
		}
	}
	if tally.mismatch == 0 || tally.unknown == 0 || tally.covered == 0 || tally.uncovered == 0 {
		t.Fatalf("property run never reached every verdict: %+v", tally)
	}
}
