package report

import (
	"context"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net/http"
	"path/filepath"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/failure"
	"github.com/gosuda/dalgona/internal/dalgona/github"
)

const (
	reportOwner    = "gosuda"
	reportRepo     = "dalgona"
	reportFullName = "gosuda/dalgona"
)

type GitHub interface {
	Repository(ctx context.Context, repo github.Repository) (github.RepositoryInfo, github.ResponseMeta, error)
	SearchIssues(ctx context.Context, query github.IssueSearch) (github.SearchResult, github.ResponseMeta, error)
	CreateIssue(ctx context.Context, input github.CreateIssue) (github.Issue, github.ResponseMeta, error)
	ReactIssue(ctx context.Context, input github.IssueReaction) (github.ResponseMeta, error)
}

type Credential struct {
	Access  string
	Expires time.Time
}

type CredentialSource interface {
	Credential(ctx context.Context) (Credential, error)
}

type Eligibility struct {
	Origin         failure.Origin
	Test           bool
	HomeOverridden bool
}

type Options struct {
	Home          string
	Client        GitHub
	Credentials   CredentialSource
	Eligibility   Eligibility
	ClientID      string
	Now           func() time.Time
	Notice        io.Writer
	ConsentMethod string
}

type QueueEntry struct {
	Fingerprint string         `json:"fingerprint"`
	Record      failure.Record `json:"record"`
	Enqueued    time.Time      `json:"enqueued"`
	Recurrences uint64         `json:"recurrences"`
}

type Receipt struct {
	Fingerprint       string    `json:"fingerprint"`
	State             string    `json:"state"`
	IssueNumber       int64     `json:"issueNumber"`
	Attempted         time.Time `json:"attempted"`
	ReactionAttempted bool      `json:"reactionAttempted"`
}

type Reporter struct {
	dir           string
	client        GitHub
	source        CredentialSource
	eligible      Eligibility
	now           func() time.Time
	notice        io.Writer
	mu            sync.Mutex
	drainMu       sync.Mutex
	enqueueMu     sync.Mutex
	queue         []QueueEntry
	state         reporterState
	wake          chan struct{}
	done          chan struct{}
	drainCtx      context.Context
	cancelDrain   context.CancelFunc
	consentMethod string
	wg            sync.WaitGroup
	release       func() error
	closeOnce     sync.Once
	closeErr      error
}

type runCaps struct {
	creates   int
	reactions int
	rests     int
	capped    bool
}

type searchOutcome struct {
	match   github.Issue
	found   bool
	blocked bool
}

type writeBudget struct {
	day    *string
	total  *int
	dayCap int
}

func Open(ctx context.Context, options Options) (*Reporter, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if options.Home == "" {
		return nil, errors.New("failure reporting home is empty")
	}
	if options.Eligibility.HomeOverridden {
		return nil, errors.New("automatic failure reporting is off for an overridden home")
	}
	now := options.Now
	if now == nil {
		now = time.Now
	}
	notice := options.Notice
	if notice == nil {
		notice = io.Discard
	}
	dir := filepath.Join(options.Home, reportDirName)
	if err := ensureReporterDir(dir); err != nil {
		return nil, err
	}
	release, err := lockReporterDir(dir)
	if err != nil {
		return nil, err
	}
	runCtx, stop := context.WithCancel(context.Background())
	r := &Reporter{
		dir:           dir,
		client:        options.Client,
		source:        options.Credentials,
		eligible:      options.Eligibility,
		consentMethod: options.ConsentMethod,
		now:           now,
		notice:        notice,
		wake:          make(chan struct{}, 1),
		done:          make(chan struct{}),
		drainCtx:      runCtx,
		cancelDrain:   stop,
		release:       release,
	}
	queue, state, err := loadReporterFiles(dir)
	if err != nil {
		stop()
		_ = release()
		return nil, err
	}
	r.queue = queue
	r.state = state
	r.startWorker()
	r.signal()
	return r, nil
}

func loadReporterFiles(dir string) ([]QueueEntry, reporterState, error) {
	queue, err := readQueueFile(dir)
	if err != nil {
		return nil, reporterState{}, err
	}
	state, err := readStateFile(dir)
	if err != nil {
		return nil, reporterState{}, err
	}
	return queue, state, nil
}

func (r *Reporter) available() error {
	if r.eligible.HomeOverridden {
		return errors.New("automatic failure reporting is off for an overridden home")
	}
	return nil
}

func (r *Reporter) signal() {
	select {
	case r.wake <- struct{}{}:
	default:
	}
}

func (r *Reporter) startWorker() {
	r.wg.Add(1)
	go func() {
		defer r.wg.Done()
		for {
			select {
			case <-r.done:
				return
			case <-r.wake:
				r.drainSignaled()
			}
		}
	}()
}

func (r *Reporter) drainSignaled() {
	if err := r.Drain(r.drainCtx); err != nil {
		r.noticeDrainError(err)
	}
}

func (r *Reporter) noticeDrainError(err error) {
	select {
	case <-r.done:
	default:
		_, _ = fmt.Fprintln(r.notice, err)
	}
}

func (r *Reporter) Close(ctx context.Context) error {
	r.closeOnce.Do(func() {
		close(r.done)
		r.cancelDrain()
		r.wg.Wait()
		r.mu.Lock()
		release := r.release
		r.release = nil
		r.mu.Unlock()
		if release != nil {
			r.closeErr = release()
		}
	})
	if err := ctx.Err(); err != nil {
		return err
	}
	return r.closeErr
}

func (r *Reporter) Enqueue(ctx context.Context, record failure.Record) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if err := r.available(); err != nil {
		return err
	}
	if !r.consentActive() {
		return nil
	}
	if record.Product != reportRepo || !record.Reportable || !validFingerprint(record.Fingerprint) {
		return nil
	}
	r.enqueueMu.Lock()
	defer r.enqueueMu.Unlock()
	r.pruneExpired()
	coalesced, err := r.coalesce(record)
	if err != nil {
		return err
	}
	if coalesced {
		r.signal()
		return nil
	}
	return r.appendEntry(record)
}

func (r *Reporter) coalesce(record failure.Record) (bool, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for i := range r.queue {
		if r.queue[i].Fingerprint != record.Fingerprint {
			continue
		}
		r.queue[i].Recurrences++
		if err := r.saveAllLocked(); err != nil {
			return true, err
		}
		return true, nil
	}
	return false, nil
}

func (r *Reporter) appendEntry(record failure.Record) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.queue = append(r.queue, QueueEntry{
		Fingerprint: record.Fingerprint,
		Record:      record,
		Enqueued:    r.now(),
	})
	if len(r.queue) > maxQueueEntries || r.queueBytes() > maxQueueBytes {
		r.queue = r.queue[:len(r.queue)-1]
		r.state.Dropped++
		if err := r.saveAllLocked(); err != nil {
			return err
		}
		return errors.New("failure reporting queue full; report kept locally")
	}
	if err := r.saveAllLocked(); err != nil {
		return err
	}
	r.signal()
	return nil
}

func (r *Reporter) Drain(ctx context.Context) error {
	r.drainMu.Lock()
	defer r.drainMu.Unlock()
	if err := ctx.Err(); err != nil {
		return err
	}
	if err := r.available(); err != nil {
		return err
	}
	ready, err := r.drainReady()
	if err != nil || !ready {
		return err
	}
	r.pruneExpired()
	run := &runCaps{}
	if err := r.drainEntries(ctx, run); err != nil {
		return err
	}
	if err := r.saveAll(); err != nil {
		return err
	}
	if run.capped && r.queueLen() > 0 {
		return errors.New("failure reporting paused: local limit reached")
	}
	return nil
}

func (r *Reporter) consentActive() bool {
	return r.consentMethod == "" || ConsentStored(filepath.Dir(r.dir))
}

func (r *Reporter) consentValid() bool {
	if r.consentMethod == "" {
		return true
	}
	home := filepath.Dir(r.dir)
	if ConsentValid(home, r.consentMethod) {
		return true
	}
	if !ConsentStored(home) {
		r.queue = nil
	}
	return false
}

func (r *Reporter) drainReady() (bool, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.client == nil || r.source == nil {
		if len(r.queue) == 0 {
			return false, nil
		}
		return false, errors.New("failure reporting client unavailable")
	}
	if r.state.StoppedStatus != 0 {
		return false, latchError(r.state.StoppedStatus)
	}
	if !r.consentValid() {
		return false, nil
	}
	if resetter, ok := r.source.(interface{ BeginRun() }); ok {
		resetter.BeginRun()
	}
	return true, nil
}

func (r *Reporter) drainEntries(ctx context.Context, run *runCaps) error {
	for _, fingerprint := range r.pendingFingerprints() {
		if err := ctx.Err(); err != nil {
			return err
		}
		if err := r.process(ctx, fingerprint, run); err != nil {
			return err
		}
	}
	return nil
}

func (r *Reporter) process(ctx context.Context, fingerprint string, run *runCaps) error {
	entry, ok := r.findEntry(fingerprint)
	if !ok {
		return nil
	}
	receipt, has := r.findReceipt(fingerprint)
	if has {
		return r.resolveReceipt(ctx, entry, receipt, run)
	}
	outcome, err := r.searchMarker(ctx, fingerprint, run)
	if err != nil || outcome.blocked {
		return err
	}
	if outcome.found {
		return r.fileFound(ctx, entry, outcome.match.Number, run)
	}
	return r.create(ctx, entry, run)
}

func (r *Reporter) resolveReceipt(ctx context.Context, entry QueueEntry, receipt Receipt, run *runCaps) error {
	switch receipt.State {
	case receiptFiled, receiptFound:
		return r.react(ctx, entry, receipt, run)
	case receiptAttempted, receiptUncertain:
		return r.resolveAttempted(ctx, entry, receipt, run)
	default:
		r.removeEntry(entry.Fingerprint)
		return r.saveAll()
	}
}

func (r *Reporter) resolveAttempted(ctx context.Context, entry QueueEntry, receipt Receipt, run *runCaps) error {
	outcome, err := r.searchMarker(ctx, entry.Fingerprint, run)
	if err != nil || outcome.blocked {
		return err
	}
	if !outcome.found {
		receipt.State = receiptUncertain
		r.storeReceipt(receipt)
		return r.saveAll()
	}
	receipt.State = receiptFound
	receipt.IssueNumber = outcome.match.Number
	r.storeReceipt(receipt)
	r.removeEntry(entry.Fingerprint)
	return r.saveAll()
}

func (r *Reporter) fileFound(ctx context.Context, entry QueueEntry, number int64, run *runCaps) error {
	r.mu.Lock()
	over := len(r.state.Receipts) >= maxReceipts
	r.mu.Unlock()
	if over {
		return errors.New("failure reporting stopped: deduplication state is full")
	}
	r.storeReceipt(Receipt{
		Fingerprint: entry.Fingerprint,
		State:       receiptFound,
		IssueNumber: number,
		Attempted:   r.now(),
	})
	if err := r.saveAll(); err != nil {
		return err
	}
	receipt, _ := r.findReceipt(entry.Fingerprint)
	return r.react(ctx, entry, receipt, run)
}

func (r *Reporter) markUncertain(entry QueueEntry) error {
	r.storeReceipt(Receipt{
		Fingerprint: entry.Fingerprint,
		State:       receiptUncertain,
		Attempted:   r.now(),
	})
	return r.saveAll()
}

func (r *Reporter) create(ctx context.Context, entry QueueEntry, run *runCaps) error {
	title, body, ok := splitReportText(failure.ReportText(entry.Record))
	if !ok {
		return r.markUncertain(entry)
	}
	r.mu.Lock()
	over := len(r.state.Receipts) >= maxReceipts
	r.mu.Unlock()
	if over {
		return errors.New("failure reporting stopped: deduplication state is full")
	}
	if !r.reserveCreate(run) {
		return nil
	}
	r.storeReceipt(Receipt{
		Fingerprint: entry.Fingerprint,
		State:       receiptAttempted,
		Attempted:   r.now(),
	})
	if err := r.saveAll(); err != nil {
		return err
	}
	issue, meta, err := r.client.CreateIssue(ctx, github.CreateIssue{
		Owner: reportOwner,
		Repo:  reportRepo,
		Title: title,
		Body:  body,
	})
	r.noteCore(meta)
	if err == nil {
		r.storeReceipt(Receipt{
			Fingerprint: entry.Fingerprint,
			State:       receiptFiled,
			IssueNumber: issue.Number,
			Attempted:   r.now(),
		})
		r.removeEntry(entry.Fingerprint)
		return r.saveAll()
	}
	if status, ok := apiStatus(err); ok && stopStatus(status) {
		return r.setLatch(status)
	}
	return r.markUncertain(entry)
}

func (r *Reporter) react(ctx context.Context, entry QueueEntry, receipt Receipt, run *runCaps) error {
	if receipt.ReactionAttempted {
		r.removeEntry(entry.Fingerprint)
		return r.saveAll()
	}
	if !r.reserveReaction(run) {
		return nil
	}
	receipt.ReactionAttempted = true
	r.storeReceipt(receipt)
	if err := r.saveAll(); err != nil {
		return err
	}
	meta, err := r.client.ReactIssue(ctx, github.IssueReaction{
		Owner:   reportOwner,
		Repo:    reportRepo,
		Number:  receipt.IssueNumber,
		Content: "eyes",
	})
	r.noteCore(meta)
	if err != nil {
		if status, ok := apiStatus(err); ok && stopStatus(status) {
			return r.setLatch(status)
		}
	}
	r.removeEntry(entry.Fingerprint)
	return r.saveAll()
}

func (r *Reporter) searchMarker(ctx context.Context, fingerprint string, run *runCaps) (searchOutcome, error) {
	var outcome searchOutcome
	query := `is:issue in:title "` + markerPrefix + fingerprint + `"`
	for page := 1; page <= maxSearchPages; page++ {
		result, blocked, err := r.fetchSearchPage(ctx, query, page, run)
		if err != nil {
			return outcome, err
		}
		if blocked {
			outcome.blocked = true
			return outcome, nil
		}
		if match, found := matchSearchItems(result.Items, fingerprint); found {
			if !outcome.found || match.Number < outcome.match.Number {
				outcome.match = match
				outcome.found = true
			}
		}
		if len(result.Items) < 100 {
			break
		}
	}
	return outcome, nil
}

func (r *Reporter) fetchSearchPage(ctx context.Context, query string, page int, run *runCaps) (github.SearchResult, bool, error) {
	var result github.SearchResult
	if !r.chargeSearch(run) {
		return result, true, nil
	}
	res, meta, err := r.client.SearchIssues(ctx, github.IssueSearch{
		Query:   query,
		Page:    page,
		PerPage: 100,
	})
	r.noteSearch(meta)
	if err != nil {
		if status, ok := apiStatus(err); ok && stopStatus(status) {
			return result, false, r.setLatch(status)
		}
		return result, true, nil
	}
	if res.Incomplete || res.TotalCount > 1000 {
		return result, true, nil
	}
	return res, false, nil
}

func matchSearchItems(items []github.Issue, fingerprint string) (github.Issue, bool) {
	var match github.Issue
	found := false
	for _, item := range items {
		parsed, ok := FingerprintFromTitle(item.Title)
		if !ok || parsed != fingerprint || item.PullRequest || item.Repository != reportFullName {
			continue
		}
		if !found || item.Number < match.Number {
			match = item
			found = true
		}
	}
	return match, found
}

func (r *Reporter) reserveCreate(run *runCaps) bool {
	return r.reserveWrite(run, writeBudget{day: &r.state.CreatesDay, total: &r.state.Creates, dayCap: maxDayCreates}, &run.creates, maxRunCreates)
}

func (r *Reporter) reserveReaction(run *runCaps) bool {
	return r.reserveWrite(run, writeBudget{day: &r.state.ReactionsDay, total: &r.state.Reactions, dayCap: maxDayReactions}, &run.reactions, maxRunReactions)
}

func (r *Reporter) reserveWrite(run *runCaps, budget writeBudget, runTotal *int, runCap int) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	now := r.now()
	if now.Sub(r.state.LastWrite) < writeSpacing {
		return false
	}
	if bucketPaused(now, r.state.CoreRemaining, r.state.CoreReset, r.state.CoreRetryAfter) {
		return false
	}
	today := now.UTC().Format("2006-01-02")
	if *budget.day != today {
		*budget.day = today
		*budget.total = 0
	}
	if *budget.total >= budget.dayCap || *runTotal >= runCap {
		run.capped = true
		return false
	}
	if !r.chargeRests(run) {
		return false
	}
	*budget.total++
	*runTotal++
	r.state.LastWrite = now
	return true
}

func (r *Reporter) chargeSearch(run *runCaps) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	if bucketPaused(r.now(), r.state.SearchRemaining, r.state.SearchReset, r.state.SearchRetryAfter) {
		return false
	}
	return r.chargeRests(run)
}

func (r *Reporter) chargeRests(run *runCaps) bool {
	today := r.now().UTC().Format("2006-01-02")
	if r.state.RestsDay != today {
		r.state.RestsDay = today
		r.state.Rests = 0
	}
	if r.state.Rests >= maxDayRests || run.rests >= maxRunRests {
		run.capped = true
		return false
	}
	r.state.Rests++
	run.rests++
	return true
}

func (r *Reporter) noteSearch(meta github.ResponseMeta) {
	if meta.Status == 0 {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	r.state.SearchRemaining = meta.Remaining
	if !meta.Reset.IsZero() {
		r.state.SearchReset = meta.Reset
	}
	if !meta.RetryAfter.IsZero() {
		r.state.SearchRetryAfter = meta.RetryAfter
	}
}

func (r *Reporter) noteCore(meta github.ResponseMeta) {
	if meta.Status == 0 {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	r.state.CoreRemaining = meta.Remaining
	if !meta.Reset.IsZero() {
		r.state.CoreReset = meta.Reset
	}
	if !meta.RetryAfter.IsZero() {
		r.state.CoreRetryAfter = meta.RetryAfter
	}
}

func (r *Reporter) setLatch(status int) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.state.StoppedStatus = status
	err := latchError(status)
	if saveErr := r.saveAllLocked(); saveErr != nil {
		return saveErr
	}
	return err
}

func (r *Reporter) pruneExpired() {
	r.mu.Lock()
	defer r.mu.Unlock()
	cutoff := r.now().Add(-queueEntryMaxAge)
	for i := range r.queue {
		if r.queue[i].Enqueued.After(cutoff) {
			continue
		}
		r.queue[i].Record.Error = ""
		r.queue[i].Record.Args = ""
		r.queue[i].Record.Output = ""
	}
}

func (r *Reporter) pendingFingerprints() []string {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make([]string, 0, len(r.queue))
	for _, entry := range r.queue {
		out = append(out, entry.Fingerprint)
	}
	return out
}

func (r *Reporter) queueLen() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return len(r.queue)
}

func (r *Reporter) findEntry(fingerprint string) (QueueEntry, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, entry := range r.queue {
		if entry.Fingerprint == fingerprint {
			return entry, true
		}
	}
	return QueueEntry{}, false
}

func (r *Reporter) findReceipt(fingerprint string) (Receipt, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, receipt := range r.state.Receipts {
		if receipt.Fingerprint == fingerprint {
			return receipt, true
		}
	}
	return Receipt{}, false
}

func (r *Reporter) storeReceipt(receipt Receipt) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for i := range r.state.Receipts {
		if r.state.Receipts[i].Fingerprint == receipt.Fingerprint {
			r.state.Receipts[i] = receipt
			return
		}
	}
	r.state.Receipts = append(r.state.Receipts, receipt)
}

func (r *Reporter) removeEntry(fingerprint string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	kept := r.queue[:0]
	for _, entry := range r.queue {
		if entry.Fingerprint != fingerprint {
			kept = append(kept, entry)
		}
	}
	r.queue = kept
}

func (r *Reporter) queueBytes() int {
	encoded, err := json.Marshal(queueFile{Entries: r.queue})
	if err != nil {
		return maxQueueBytes + 1
	}
	return len(encoded)
}

func (r *Reporter) saveAll() error {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.saveAllLocked()
}

func (r *Reporter) saveAllLocked() error {
	queueEncoded, err := json.Marshal(queueFile{Entries: r.queue})
	if err != nil {
		return fmt.Errorf("failure reporting write local state: %w", err)
	}
	stateEncoded, err := json.Marshal(r.state)
	if err != nil {
		return fmt.Errorf("failure reporting write local state: %w", err)
	}
	if len(stateEncoded) > maxStateBytes {
		return errors.New("failure reporting stopped: deduplication state is full")
	}
	if err := atomicWriteFile(r.dir, queueFileName, queueEncoded); err != nil {
		return err
	}
	return atomicWriteFile(r.dir, stateFileName, stateEncoded)
}

func bucketPaused(now time.Time, remaining int, reset, retryAfter time.Time) bool {
	if !retryAfter.IsZero() && now.Before(retryAfter) {
		return true
	}
	return remaining == 0 && !reset.IsZero() && now.Before(reset.Add(time.Second))
}

func stopStatus(status int) bool {
	switch status {
	case http.StatusUnauthorized, http.StatusForbidden, http.StatusTooManyRequests,
		http.StatusNotFound, http.StatusGone, http.StatusUnprocessableEntity:
		return true
	default:
		return false
	}
}

func latchError(status int) error {
	if status == http.StatusNotFound || status == http.StatusGone || status == http.StatusUnprocessableEntity {
		return fmt.Errorf("failure reporting stopped after HTTP %d; check repository access and issue policy", status)
	}
	return fmt.Errorf("failure reporting stopped after HTTP %d; review access before enabling again", status)
}

func apiStatus(err error) (int, bool) {
	apiErr, ok := errors.AsType[*github.APIError](err)
	if !ok {
		return 0, false
	}
	return apiErr.Status, true
}

func NewReporterClient(baseURL string, transport *http.Client, version string, source CredentialSource) (*github.Client, error) {
	if source == nil {
		return nil, errors.New("failure reporting client unavailable")
	}
	if version == "" {
		version = "unknown"
	}
	roundTripper := http.DefaultTransport
	if transport != nil && transport.Transport != nil {
		roundTripper = transport.Transport
	}
	resolve := func(ctx context.Context) (string, error) {
		credential, err := source.Credential(ctx)
		if err != nil {
			return "", err
		}
		if !credential.Expires.IsZero() && !time.Now().Before(credential.Expires) {
			return "", errors.New("failure reporting credential expired")
		}
		return credential.Access, nil
	}
	return github.NewClient(github.ClientConfig{
		BaseURL:      baseURL,
		Repositories: []github.Repository{{Owner: reportOwner, Name: reportRepo}},
		Policy:       "reporter",
	}, github.NewUserCredential(resolve), &http.Client{Transport: userAgentTransport{
		base:    roundTripper,
		version: version,
	}})
}

type userAgentTransport struct {
	base    http.RoundTripper
	version string
}

func (t userAgentTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	duplicate := req.Clone(req.Context())
	duplicate.Header.Set("User-Agent", "dalgona/"+t.version)
	return t.base.RoundTrip(duplicate)
}
