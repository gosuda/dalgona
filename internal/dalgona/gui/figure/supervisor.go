package figure

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"math"
	"os"
	"slices"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document/code"
)

var (
	ErrWaiting     = errors.New("waiting to render: the figure queue is full; the figure renders when earlier figures finish")
	ErrPaused      = errors.New("rendering is paused after repeated renderer failures; choose Resume rendering to continue")
	ErrFailedInput = errors.New("this figure failed before; change its source or choose Retry")
	ErrClosed      = errors.New("the figure renderer is closed")
)

type SupervisorConfig struct {
	Executable string
	Args       []string
	Fonts      Fonts
}

type Request struct {
	Document string
	Revision uint64
	Kind     Kind
	Source   []byte
	Theme    Theme
	Scale    float64
	Viewport [2]float64
	Visible  bool
	Retry    bool
}

type ImageRequest struct {
	Format        ImageFormat
	Data          []byte
	Scale         float64
	Viewport      [2]float64
	Visible       bool
	ReleaseSource func()
}

type HighlightRequest struct {
	Document string
	Revision uint64
	Language string
	Source   []byte
	Visible  bool
}

type Token struct {
	Start int
	End   int
	Type  uint32
}

type Preview struct {
	Width        int
	Height       int
	SourceWidth  int
	SourceHeight int
	Tiles        []Tile
}

type Outcome struct {
	Job     uint64
	Key     string
	List    *List
	Image   *Preview
	Preview PreviewKey
	Tokens  []Token
	Err     *Failure
}

type job struct {
	id       uint64
	key      string
	visible  bool
	request  envelope
	payload  []byte
	preview  PreviewKey
	cancel   chan struct{}
	canceled bool

	clipboard     clipboardimage.Offer
	releaseSource func()
}

type imageResult struct {
	key     PreviewKey
	outcome Outcome
}

type Supervisor struct {
	cfg      SupervisorConfig
	mu       sync.Mutex
	wake     chan struct{}
	queue    []*job
	running  *job
	next     uint64
	failures []time.Time
	failed   map[string]int
	paused   bool
	closed   bool
	worker   *workerProcess
	launches int
	outcomes chan Outcome
	pixels   *PixelCache
	images   map[uint64]imageResult
	refs     map[PreviewKey]int
	fonts    string
	done     chan struct{}
	stop     chan struct{}

	copyID      uint64
	copyOffer   clipboardimage.Offer
	copyOutcome Outcome
}

func NewSupervisor(cfg SupervisorConfig) (*Supervisor, error) {
	if cfg.Executable == "" {
		exe, err := os.Executable()
		if err != nil {
			return nil, err
		}
		cfg.Executable = exe
	}
	if cfg.Args == nil {
		cfg.Args = []string{ChildArgument}
	}
	s := &Supervisor{
		cfg:      cfg,
		wake:     make(chan struct{}, 1),
		failed:   map[string]int{},
		outcomes: make(chan Outcome, PendingJobLimit),
		pixels:   NewPixelCache(SharedPixels),
		images:   map[uint64]imageResult{},
		refs:     map[PreviewKey]int{},
		fonts:    cfg.Fonts.manifest(),
		done:     make(chan struct{}),
		stop:     make(chan struct{}),
	}
	go s.run()
	return s, nil
}

func (s *Supervisor) Pixels() *PixelCache { return s.pixels }

func (s *Supervisor) Outcomes() <-chan Outcome { return s.outcomes }

func (s *Supervisor) ContentKey(r Request) string {
	h := sha256.New()
	h.Write(EngineManifest[:])
	h.Write([]byte(s.fonts))
	h.Write([]byte{byte(r.Kind)})
	for _, role := range themeRoles(r.Theme) {
		h.Write([]byte(role))
	}
	for _, v := range [...]float64{r.Scale, r.Viewport[0], r.Viewport[1]} {
		h.Write(binary.LittleEndian.AppendUint64(nil, math.Float64bits(v)))
	}
	h.Write(r.Source)
	return hex.EncodeToString(h.Sum(nil))
}

func (s *Supervisor) Render(r Request) (uint64, string, error) {
	if len(r.Source) > SourceLimit {
		return 0, "", fail(FailureLimit, "figure source has %d bytes; the limit is %d", len(r.Source), SourceLimit)
	}
	key := s.ContentKey(r)
	env := envelope{
		Type: messageRender, Document: r.Document, Revision: r.Revision, Key: key, Kind: r.Kind.String(),
		Theme: themeRoles(r.Theme), Scale: r.Scale, Viewport: []float64{r.Viewport[0], r.Viewport[1]},
	}
	id, err := s.submit(&job{key: key, visible: r.Visible, request: env, payload: r.Source}, r.Retry)
	return id, key, err
}

func (s *Supervisor) Image(r ImageRequest) (uint64, string, error) {
	if len(r.Data) > ImageEncodedLimit {
		return 0, "", fail(FailureLimit, "image has %d bytes; the limit is %d", len(r.Data), ImageEncodedLimit)
	}
	preview := PreviewKeyOf(r)
	side := preview.Side
	if side < 0 {
		return 0, "", fail(FailureLimit, "image side %d is negative; send a viewport with a non-negative scale", side)
	}
	key := digestOf(preview.Image[:], []byte(r.Format.String()), binary.LittleEndian.AppendUint64(nil, uint64(side)))
	env := envelope{Type: messageImage, Key: key, Format: r.Format.String(), Scale: r.Scale, Viewport: []float64{r.Viewport[0], r.Viewport[1]}}
	id, err := s.submit(&job{key: key, visible: r.Visible, request: env, payload: r.Data, preview: preview, releaseSource: r.ReleaseSource}, false)
	return id, key, err
}

func (s *Supervisor) ExportClipboardImage(format ImageFormat, data []byte, offer clipboardimage.Offer, releaseSource func()) (uint64, error) {
	if len(data) > ImageEncodedLimit {
		return 0, fail(FailureLimit, "image has %d bytes; the limit is %d", len(data), ImageEncodedLimit)
	}
	kind := clipboardimage.PNG
	if offer.NeedsDIB() {
		kind = clipboardPNGDIB
	}
	key := clipboardKey(kind, format.String(), data)
	env := envelope{Type: messageClipboardImage, Kind: kind, Key: key, Format: format.String()}
	return s.submit(&job{key: key, visible: true, request: env, payload: data, clipboard: offer, releaseSource: releaseSource}, false)
}

func (s *Supervisor) TakeClipboardImage(id uint64) (clipboardimage.Offer, error, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.copyOutcome.Job != id || id == 0 {
		return nil, nil, false
	}
	offer, o := s.copyOffer, s.copyOutcome
	s.copyID, s.copyOffer, s.copyOutcome = 0, nil, Outcome{}
	if o.Err != nil {
		return nil, o.Err, true
	}
	return offer, nil, true
}

func (s *Supervisor) ImageResult(id uint64) (Outcome, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	o := s.images[id].outcome
	return o, o.Job != 0
}

func (s *Supervisor) ReleaseImage(id uint64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	img, ok := s.images[id]
	if !ok {
		return
	}
	s.cancel(id)
	delete(s.images, id)
	s.refs[img.key]--
	if s.refs[img.key] == 0 {
		delete(s.refs, img.key)
		s.pixels.releasePreview(img.key)
	}
}

func (s *Supervisor) Highlight(r HighlightRequest) (uint64, string, error) {
	if len(r.Source) > HighlightSourceLimit {
		return 0, "", fail(FailureLimit, "code has %d bytes; the highlighting limit is %d", len(r.Source), HighlightSourceLimit)
	}
	language, known := code.Normalize(r.Language)
	if !known {
		return 0, "", fail(FailureInvalid, "no highlighter is registered for %q", r.Language)
	}
	key := digestOf([]byte(language), []byte{0}, r.Source)
	env := envelope{Type: messageHighlight, Document: r.Document, Revision: r.Revision, Key: key, Language: language}
	id, err := s.submit(&job{key: key, visible: r.Visible, request: env, payload: r.Source}, false)
	return id, key, err
}

func (s *Supervisor) submit(j *job, retry bool) (uint64, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.jobAdmission(j); err != nil {
		return 0, err
	}
	if used, failed := s.failed[j.key]; failed {
		if !retry || used > 0 {
			return 0, ErrFailedInput
		}
		s.failed[j.key] = 1
	}
	s.next++
	j.id = s.next
	j.request.Job = j.id
	j.request.DeadlineMs = RenderDeadline.Milliseconds()
	j.cancel = make(chan struct{})
	if j.request.Type == messageImage {
		s.images[j.id] = imageResult{key: j.preview}
		s.refs[j.preview]++
	} else if j.clipboard != nil {
		s.copyID = j.id
	}
	s.queue = append(s.queue, j)
	s.signal()
	return j.id, nil
}

func (s *Supervisor) jobAdmission(j *job) error {
	switch {
	case s.closed:
		return ErrClosed
	case s.paused:
		return ErrPaused
	case len(s.queue)+boolCount(s.running != nil) >= PendingJobLimit:
		return ErrWaiting
	case j.clipboard != nil && s.copyID != 0:
		return ErrWaiting
	}
	return nil
}

func boolCount(v bool) int {
	if v {
		return 1
	}
	return 0
}

func (s *Supervisor) signal() {
	select {
	case s.wake <- struct{}{}:
	default:
	}
}

func (s *Supervisor) Cancel(id uint64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.cancel(id)
}

func (s *Supervisor) cancel(id uint64) {
	if id != 0 && s.copyID == id {
		s.discardClipboardImage()
	}
	s.queue = slices.DeleteFunc(s.queue, func(j *job) bool {
		if j.id != id {
			return false
		}
		j.closeClipboardImage()
		j.releaseInput()
		return true
	})
	if s.running != nil && s.running.id == id && !s.running.canceled {
		s.running.canceled = true
		close(s.running.cancel)
	}
}

func (s *Supervisor) discardClipboardImage() {
	if s.copyOffer != nil {
		s.copyOffer.Close()
	}
	s.copyID, s.copyOffer, s.copyOutcome = 0, nil, Outcome{}
}

func (j *job) releaseInput() {
	if j.releaseSource != nil {
		j.payload = nil
		j.releaseSource()
		j.releaseSource = nil
	}
}

func (j *job) closeClipboardImage() {
	if j.clipboard != nil {
		j.clipboard.Close()
	}
}

func (s *Supervisor) Paused() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.paused
}

func (s *Supervisor) Resume() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.paused = false
	s.failures = nil
	s.signal()
}

func (s *Supervisor) Launches() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.launches
}

func (s *Supervisor) Close() {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return
	}
	s.closed = true
	close(s.stop)
	for key := range s.refs {
		s.pixels.releasePreview(key)
	}
	clear(s.refs)
	clear(s.images)
	s.cancel(s.copyID)
	for _, queued := range s.queue {
		queued.releaseInput()
	}
	s.queue = nil
	if s.running != nil && !s.running.canceled {
		s.running.canceled = true
		close(s.running.cancel)
	}
	s.signal()
	s.mu.Unlock()
	<-s.done
}

func (s *Supervisor) take() *job {
	for {
		s.mu.Lock()
		if s.closed {
			s.mu.Unlock()
			return nil
		}
		if !s.paused && len(s.queue) > 0 {
			i := slices.IndexFunc(s.queue, func(j *job) bool { return j.visible })
			i = max(i, 0)
			j := s.queue[i]
			s.queue = slices.Delete(s.queue, i, i+1)
			s.running = j
			s.mu.Unlock()
			return j
		}
		s.mu.Unlock()
		<-s.wake
	}
}

func (s *Supervisor) run() {
	defer close(s.done)
	defer close(s.outcomes)
	defer func() {
		if s.worker != nil {
			s.worker.terminate()
		}
	}()
	for {
		j := s.take()
		if j == nil {
			return
		}
		outcome, publish := s.finish(j, s.execute(j))
		if !publish {
			continue
		}
		select {
		case s.outcomes <- outcome:
		case <-s.stop:
			return
		}
	}
}

func (s *Supervisor) finish(j *job, outcome Outcome) (Outcome, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.running = nil
	j.releaseInput()
	if s.closed || j.canceled {
		j.closeClipboardImage()
		return Outcome{}, false
	}
	outcome = s.admit(j, outcome)
	switch {
	case outcome.Err == nil:
		delete(s.failed, j.key)
	case outcome.Err.Code != FailureCancelled && j.clipboard == nil:
		s.failed[j.key] = max(s.failed[j.key], 0)
		trimFailedKeys(s.failed)
	}
	if j.request.Type == messageImage {
		s.images[j.id] = imageResult{key: j.preview, outcome: outcome}
	} else if j.clipboard != nil {
		s.copyOutcome = outcome
		if outcome.Err == nil {
			s.copyOffer = j.clipboard
		} else {
			j.closeClipboardImage()
		}
	}
	return outcome, true
}

func (s *Supervisor) admit(j *job, outcome Outcome) Outcome {
	if outcome.Image == nil {
		return outcome
	}
	if err := s.pixels.Admit(j.preview, outcome.Image); err != nil {
		return Outcome{Job: outcome.Job, Key: outcome.Key, Err: FailureOf(err)}
	}
	outcome.Image, outcome.Preview = nil, j.preview
	return outcome
}

const failedKeyLimit = 256

func trimFailedKeys(failed map[string]int) {
	for key := range failed {
		if len(failed) <= failedKeyLimit {
			return
		}
		delete(failed, key)
	}
}

func (s *Supervisor) recordFailure() {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := time.Now()
	s.failures = slices.DeleteFunc(s.failures, func(t time.Time) bool { return now.Sub(t) > FailureWindow })
	s.failures = append(s.failures, now)
	if len(s.failures) >= FailureWindowLimit {
		s.paused = true
	}
}

func (s *Supervisor) ensureWorker() error {
	if s.worker != nil && s.worker.alive() {
		return nil
	}
	if s.worker != nil {
		s.worker.terminate()
		s.worker = nil
	}
	w, err := startWorker(s.cfg.Executable, s.cfg.Args)
	if err != nil {
		return err
	}
	s.mu.Lock()
	s.launches++
	s.mu.Unlock()
	if err := w.handshake(helloTimeout, s.fonts); err != nil {
		w.terminate()
		return err
	}
	s.worker = w
	return nil
}

func (s *Supervisor) execute(j *job) Outcome {
	if err := s.ensureWorker(); err != nil {
		s.recordFailure()
		return Outcome{Job: j.id, Key: j.key, Err: FailureOf(err)}
	}
	outcome, healthy := s.warmUp(j)
	if healthy {
		outcome, healthy = s.exchange(j, RenderDeadline)
	}
	if !healthy {
		s.worker.terminate()
		s.worker = nil
		if outcome.Err == nil || outcome.Err.Code != FailureCancelled {
			s.recordFailure()
		}
	}
	return outcome
}

const warmupDeadline = 60 * time.Second

var warmupSources = map[string]string{
	KindMermaid.String(): "graph LR\nA-->B",
	KindNomnoml.String(): "[A]->[B]",
}

func (s *Supervisor) warmUp(j *job) (Outcome, bool) {
	source, scripted := warmupSources[j.request.Kind]
	if j.request.Type != messageRender || !scripted || s.worker.warm[j.request.Kind] {
		return Outcome{}, true
	}
	request := j.request
	request.Job, request.Key, request.DeadlineMs = 0, "warm-up", warmupDeadline.Milliseconds()
	outcome, healthy := s.exchange(&job{key: request.Key, request: request, payload: []byte(source), cancel: j.cancel}, warmupDeadline)
	if healthy && outcome.Err == nil {
		s.worker.warm[j.request.Kind] = true
		return outcome, true
	}
	outcome.Job, outcome.Key = j.id, j.key
	if outcome.Err == nil {
		outcome.Err = fail(FailureEngine, "%s engine did not start", j.request.Kind)
	}
	return outcome, false
}

type requestSend struct {
	err error
}

func (s *Supervisor) exchange(j *job, limit time.Duration) (Outcome, bool) {
	due := time.Now().Add(limit + TerminationGrace)
	stdin := s.worker.stdin
	sent := make(chan requestSend, 1)
	go func() { sent <- requestSend{err: writeFrame(stdin, &j.request, j.payload, requestFrameLimit)} }()
	timer := time.NewTimer(time.Until(due))
	defer timer.Stop()
	select {
	case write := <-sent:
		if write.err != nil {
			return Outcome{Job: j.id, Key: j.key, Err: FailureOf(write.err)}, false
		}
	case <-j.cancel:
		s.stopLeasedRequest(j, sent)
		return Outcome{Job: j.id, Key: j.key, Err: fail(FailureCancelled, "figure rendering was cancelled")}, false
	case <-timer.C:
		s.stopLeasedRequest(j, sent)
		return Outcome{Job: j.id, Key: j.key, Err: fail(FailureTimeout, "figure did not finish within %s", limit)}, false
	}
	return s.await(j, limit, due)
}

func (s *Supervisor) stopLeasedRequest(j *job, sent <-chan requestSend) {
	if j.releaseSource != nil {
		s.worker.terminate()
		<-sent
	}
}

func (s *Supervisor) await(j *job, limit time.Duration, due time.Time) (Outcome, bool) {
	deadline := time.NewTimer(time.Until(due))
	defer deadline.Stop()
	sample := time.NewTicker(MemorySamplePeriod)
	defer sample.Stop()
	c := newCollector(j)
	for {
		select {
		case f := <-s.worker.frames:
			if done, healthy := s.worker.collect(c, f); done {
				return c.outcome(), healthy
			}
		case <-s.worker.exited:
			return c.failure(fail(FailureEngine, "figure renderer stopped: %s", firstLine(s.worker.stderr.String()))), false
		case <-deadline.C:
			return c.failure(fail(FailureTimeout, "figure did not finish within %s", limit)), false
		case <-j.cancel:
			return c.failure(fail(FailureCancelled, "figure rendering was cancelled")), false
		case <-sample.C:
			if err := s.memoryBreach(); err != nil {
				return c.failure(err), false
			}
		}
	}
}

func (s *Supervisor) memoryBreach() error {
	if rss, ok := residentBytes(s.worker.cmd.Process.Pid); ok && rss > ChildMemoryLimit {
		return fail(FailureLimit, "figure renderer used %d bytes of memory; the threshold is %d", rss, ChildMemoryLimit)
	}
	return nil
}
