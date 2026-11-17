package daltoki

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/url"
	"regexp"
	"strconv"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dalgona/github"
)

const (
	pollOverlap        = 60 * time.Second
	pollPageLimit      = 100
	pollETagLimit      = 1000
	pollBoundaryLimit  = 10000
	reconciliationWait = 900 * time.Second
)

var errBoundaryOverflow = errors.New("daltoki: polling boundary exceeds 10000 events")

type EndpointETag struct {
	URL  string `json:"url"`
	ETag string `json:"etag"`
}

type PollCursor struct {
	Since         time.Time      `json:"since"`
	BoundaryIDs   []int64        `json:"boundaryIds"`
	ETags         []EndpointETag `json:"etags"`
	NextURL       string         `json:"nextUrl"`
	ScanStartedAt time.Time      `json:"scanStartedAt"`
	IssueWorklist []int64        `json:"issueWorklist"`
}

type Observation struct {
	Kind        string
	Repository  github.Repository
	ID          string
	IssueNumber int64
	Actor       string
	ActorType   string
	Action      string
	Labels      []string
	State       string
	Title       string
	Body        string
	URL         string
	Ignored     bool
	CreatedAt   time.Time
	UpdatedAt   time.Time
}

func (o Observation) DedupKey() string {
	stamp := o.UpdatedAt
	if stamp.IsZero() {
		stamp = o.CreatedAt
	}
	return o.Kind + "|" + o.Repository.Owner + "/" + o.Repository.Name + "|" + o.ID + "|" + strconv.FormatInt(stamp.Unix(), 10)
}

type PollStore interface {
	LoadCursor(ctx context.Context) (PollCursor, bool, error)
	SaveCursor(ctx context.Context, cursor PollCursor) error
	Admit(ctx context.Context, observation Observation) (bool, error)
}

type PollerConfig struct {
	BaseURL      string
	Repositories []github.Repository
	Interval     time.Duration
}

type Poller struct {
	client *github.Client
	store  PollStore
	config PollerConfig
	clock  func() time.Time
	logger *slog.Logger
}

func NewPoller(client *github.Client, store PollStore, config PollerConfig, clock func() time.Time, logger *slog.Logger) *Poller {
	if clock == nil {
		clock = time.Now
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &Poller{client: client, store: store, config: config, clock: clock, logger: logger}
}

type PollResult struct {
	Cursor   PollCursor
	Complete bool
	Admitted int
	Wait     time.Duration
}

type scanState struct {
	cursor     PollCursor
	boundary   map[int64]bool
	etags      map[string]string
	etagsOrder []string
	seen       map[int64]bool
	pages      int
	admitted   int
	wait       time.Duration
	now        time.Time
}

func pruneBoundary(ids []int64, seen map[int64]bool) []int64 {
	kept := ids[:0]
	for _, id := range ids {
		if seen[id] {
			kept = append(kept, id)
		}
	}
	return kept
}

func (s *scanState) observe(meta github.ResponseMeta) {
	if meta.PollInterval > s.wait {
		s.wait = meta.PollInterval
	}
	if !meta.RetryAfter.IsZero() {
		if d := meta.RetryAfter.Sub(s.now); d > s.wait {
			s.wait = d
		}
	}
	if meta.Limit > 0 && meta.Remaining == 0 && meta.Reset.After(s.now) {
		if d := meta.Reset.Sub(s.now); d > s.wait {
			s.wait = d
		}
	}
}

func (s *scanState) budgetLeft() bool {
	return s.pages < pollPageLimit
}

func (s *scanState) rememberETag(requestURL, etag string) {
	if etag == "" {
		return
	}
	if _, ok := s.etags[requestURL]; !ok {
		if len(s.etagsOrder) >= pollETagLimit {
			oldest := s.etagsOrder[0]
			s.etagsOrder = s.etagsOrder[1:]
			delete(s.etags, oldest)
		}
		s.etagsOrder = append(s.etagsOrder, requestURL)
	}
	s.etags[requestURL] = etag
	s.cursor.ETags = nil
	for _, u := range s.etagsOrder {
		s.cursor.ETags = append(s.cursor.ETags, EndpointETag{URL: u, ETag: s.etags[u]})
	}
}

func (p *Poller) Pass(ctx context.Context) (PollResult, error) {
	cursor, found, err := p.store.LoadCursor(ctx)
	if err != nil {
		return PollResult{}, err
	}
	state := p.newScan(cursor, found)
	resume, err := parsePhase(cursor.NextURL)
	if err != nil {
		return PollResult{}, err
	}
	complete, err := p.runTargets(ctx, state, resume)
	if err != nil {
		return PollResult{}, err
	}
	if !complete {
		if err := p.store.SaveCursor(ctx, state.cursor); err != nil {
			return PollResult{}, err
		}
		return PollResult{Cursor: state.cursor, Complete: false, Admitted: state.admitted, Wait: state.wait}, nil
	}
	state.cursor.Since = state.cursor.ScanStartedAt
	state.cursor.NextURL = ""
	state.cursor.BoundaryIDs = pruneBoundary(state.cursor.BoundaryIDs, state.seen)
	if err := p.store.SaveCursor(ctx, state.cursor); err != nil {
		return PollResult{}, err
	}
	return PollResult{Cursor: state.cursor, Complete: true, Admitted: state.admitted, Wait: state.wait}, nil
}

func (p *Poller) newScan(cursor PollCursor, found bool) *scanState {
	now := p.clock()
	if !found {
		cursor = PollCursor{Since: now, ScanStartedAt: now}
	}
	if found && cursor.NextURL == "" {
		cursor.ScanStartedAt = now
	}
	state := &scanState{
		cursor:   cursor,
		boundary: make(map[int64]bool, len(cursor.BoundaryIDs)),
		etags:    make(map[string]string, len(cursor.ETags)),
		seen:     make(map[int64]bool, len(cursor.BoundaryIDs)),
		now:      now,
	}
	for _, id := range cursor.BoundaryIDs {
		state.boundary[id] = true
	}
	for _, entry := range cursor.ETags {
		state.etags[entry.URL] = entry.ETag
		state.etagsOrder = append(state.etagsOrder, entry.URL)
	}
	if state.wait < p.config.Interval {
		state.wait = p.config.Interval
	}
	return state
}

func (p *Poller) runTargets(ctx context.Context, state *scanState, resume scanTarget) (bool, error) {
	for _, target := range p.scanOrder(resume) {
		if !state.budgetLeft() {
			state.cursor.NextURL = p.continuationURL(ctx, state, target)
			return false, nil
		}
		done, err := p.runPhase(ctx, state, target)
		if err != nil {
			return false, err
		}
		if !done {
			return false, nil
		}
	}
	return true, nil
}

type scanTarget struct {
	kind  string
	repo  github.Repository
	issue int64
}

func (p *Poller) scanOrder(resume scanTarget) []scanTarget {
	start := 0
	for i, r := range p.config.Repositories {
		if r == resume.repo {
			start = i
			break
		}
	}
	var targets []scanTarget
	repos := p.config.Repositories
	for offset := range repos {
		targets = append(targets, p.targetsForRepo(resume, repos[(start+offset)%len(repos)])...)
	}
	return targets
}

func (p *Poller) targetsForRepo(resume scanTarget, r github.Repository) []scanTarget {
	if r != resume.repo {
		return []scanTarget{
			{kind: "issues", repo: r},
			{kind: "comments", repo: r},
			{kind: "events", repo: r},
		}
	}
	switch resume.kind {
	case "issues":
		return []scanTarget{
			{kind: "issues", repo: r},
			{kind: "comments", repo: r},
			{kind: "events", repo: r},
		}
	case "comments":
		return []scanTarget{
			{kind: "comments", repo: r},
			{kind: "events", repo: r},
		}
	case "events":
		return []scanTarget{{kind: "events", repo: r}}
	default:
		return []scanTarget{
			{kind: "issues", repo: r},
			{kind: "comments", repo: r},
			{kind: "events", repo: r},
		}
	}
}

func (p *Poller) continuationURL(ctx context.Context, state *scanState, target scanTarget) string {
	if state.cursor.NextURL != "" && !isContinuation(state.cursor.NextURL) {
		return state.cursor.NextURL
	}
	switch target.kind {
	case "issues":
		return p.pageURL(target.repo, "issues", 0, state.cursor.Since)
	case "comments":
		return p.pageURL(target.repo, "comments", 0, state.cursor.Since)
	default:
		return eventsContinuation(target.repo)
	}
}

func isContinuation(raw string) bool {
	return strings.HasPrefix(raw, "daltoki://")
}

func eventsContinuation(repo github.Repository) string {
	return "daltoki://events/" + repo.Owner + "/" + repo.Name
}

var (
	eventsPathPattern   = regexp.MustCompile(`^/repos/([^/]+)/([^/]+)/issues/(\d+)/events$`)
	issuesPathPattern   = regexp.MustCompile(`^/repos/([^/]+)/([^/]+)/issues$`)
	commentsPathPattern = regexp.MustCompile(`^/repos/([^/]+)/([^/]+)/issues/comments$`)
)

func parsePhase(nextURL string) (scanTarget, error) {
	if nextURL == "" {
		return scanTarget{}, nil
	}
	if isContinuation(nextURL) {
		parts := strings.Split(strings.TrimPrefix(nextURL, "daltoki://"), "/")
		if len(parts) == 3 && parts[0] == "events" && parts[1] != "" && parts[2] != "" {
			return scanTarget{kind: "events", repo: github.Repository{Owner: parts[1], Name: parts[2]}}, nil
		}
		return scanTarget{}, fmt.Errorf("daltoki: invalid poll cursor")
	}
	parsed, err := url.Parse(nextURL)
	if err != nil {
		return scanTarget{}, fmt.Errorf("daltoki: invalid poll cursor")
	}
	target, ok := phaseFromPath(parsed.Path)
	if !ok {
		return scanTarget{}, fmt.Errorf("daltoki: invalid poll cursor")
	}
	return target, nil
}

func phaseFromPath(path string) (scanTarget, bool) {
	if match := eventsPathPattern.FindStringSubmatch(path); match != nil {
		number, err := strconv.ParseInt(match[3], 10, 64)
		if err != nil {
			return scanTarget{}, false
		}
		return scanTarget{kind: "events", repo: github.Repository{Owner: match[1], Name: match[2]}, issue: number}, true
	}
	if match := commentsPathPattern.FindStringSubmatch(path); match != nil {
		return scanTarget{kind: "comments", repo: github.Repository{Owner: match[1], Name: match[2]}}, true
	}
	if match := issuesPathPattern.FindStringSubmatch(path); match != nil {
		return scanTarget{kind: "issues", repo: github.Repository{Owner: match[1], Name: match[2]}}, true
	}
	return scanTarget{}, false
}

func (p *Poller) pageURL(repo github.Repository, kind string, issue int64, since time.Time) string {
	params := url.Values{}
	params.Set("per_page", "100")
	path := "/repos/" + repo.Owner + "/" + repo.Name
	switch kind {
	case "issues":
		path += "/issues"
		params.Set("state", "all")
		setSince(params, since.Add(-pollOverlap))
	case "comments":
		path += "/issues/comments"
		setSince(params, since.Add(-pollOverlap))
	case "events":
		path += "/issues/" + strconv.FormatInt(issue, 10) + "/events"
	}
	return strings.TrimRight(p.config.BaseURL, "/") + path + "?" + params.Encode()
}

func setSince(params url.Values, since time.Time) {
	if !since.IsZero() {
		params.Set("since", since.UTC().Format(time.RFC3339))
	}
}

func (p *Poller) runPhase(ctx context.Context, state *scanState, target scanTarget) (bool, error) {
	switch target.kind {
	case "issues":
		return p.issuesPhase(ctx, state, target.repo)
	case "comments":
		return p.commentsPhase(ctx, state, target.repo)
	default:
		return p.eventsPhase(ctx, state, target.repo)
	}
}

func sameEndpointPage(requestURL, kind string, repo github.Repository) bool {
	parsed, err := url.Parse(requestURL)
	if err != nil {
		return false
	}
	prefix := "/repos/" + repo.Owner + "/" + repo.Name
	if !strings.HasPrefix(parsed.Path, prefix) {
		return false
	}
	rest := strings.TrimPrefix(parsed.Path, prefix)
	if kind == "issues" && rest == "/issues" {
		return true
	}
	return kind == "comments" && rest == "/issues/comments"
}

func (p *Poller) issuesPhase(ctx context.Context, state *scanState, repo github.Repository) (bool, error) {
	first := p.pageURL(repo, "issues", 0, state.cursor.Since)
	requestURL := state.cursor.NextURL
	if requestURL == "" || isContinuation(requestURL) || !sameEndpointPage(requestURL, "issues", repo) {
		requestURL = first
	}
	for state.budgetLeft() {
		page, meta, err := p.client.Issues(ctx, github.IssuesRequest{
			Repository: repo,
			NextURL:    requestURL,
			ETag:       state.etags[requestURL],
			Since:      state.cursor.Since.Add(-pollOverlap),
		})
		state.pages++
		if err != nil {
			return false, err
		}
		state.observe(meta)
		state.rememberETag(requestURL, meta.ETag)
		if err := p.admitIssues(ctx, state, repo, page.Items); err != nil {
			return false, err
		}
		if meta.NextURL == "" {
			state.cursor.NextURL = p.pageURL(repo, "comments", 0, state.cursor.Since)
			return true, nil
		}
		state.cursor.NextURL = meta.NextURL
		requestURL = meta.NextURL
	}
	state.cursor.NextURL = requestURL
	return false, nil
}

func (p *Poller) commentsPhase(ctx context.Context, state *scanState, repo github.Repository) (bool, error) {
	first := p.pageURL(repo, "comments", 0, state.cursor.Since)
	requestURL := state.cursor.NextURL
	if requestURL == "" || isContinuation(requestURL) || !sameEndpointPage(requestURL, "comments", repo) {
		requestURL = first
	}
	for state.budgetLeft() {
		page, meta, err := p.client.Comments(ctx, github.CommentsRequest{
			Repository: repo,
			NextURL:    requestURL,
			ETag:       state.etags[requestURL],
			Since:      state.cursor.Since.Add(-pollOverlap),
		})
		state.pages++
		if err != nil {
			return false, err
		}
		state.observe(meta)
		state.rememberETag(requestURL, meta.ETag)
		if err := p.admitComments(ctx, state, repo, page.Items); err != nil {
			return false, err
		}
		if meta.NextURL == "" {
			state.cursor.NextURL = eventsContinuation(repo)
			return true, nil
		}
		state.cursor.NextURL = meta.NextURL
		requestURL = meta.NextURL
	}
	state.cursor.NextURL = requestURL
	return false, nil
}

func (p *Poller) eventsPhase(ctx context.Context, state *scanState, repo github.Repository) (bool, error) {
	numbers, err := p.labelledIssueNumbers(ctx, state, repo)
	if err != nil {
		return false, err
	}
	start, resume, err := eventResume(numbers, state.cursor.NextURL)
	if err != nil {
		return false, err
	}
	for i := start; i < len(numbers); i++ {
		number := numbers[i]
		requestURL := p.pageURL(repo, "events", number, state.cursor.Since)
		if i == start && resume != "" {
			requestURL = resume
		}
		next, done, err := p.drainIssueEvents(ctx, state, repo, number, requestURL)
		if err != nil {
			return false, err
		}
		if !done {
			state.cursor.NextURL = next
			return false, nil
		}
		state.cursor.NextURL = ""
		if i+1 < len(numbers) {
			state.cursor.NextURL = p.pageURL(repo, "events", numbers[i+1], state.cursor.Since)
		}
	}
	return true, nil
}

func eventResume(numbers []int64, nextURL string) (int, string, error) {
	if nextURL == "" || isContinuation(nextURL) {
		return 0, "", nil
	}
	resume, err := parsePhase(nextURL)
	if err != nil {
		return 0, "", err
	}
	for i, number := range numbers {
		if number == resume.issue {
			return i, nextURL, nil
		}
	}
	return 0, "", nil
}

func (p *Poller) drainIssueEvents(ctx context.Context, state *scanState, repo github.Repository, number int64, requestURL string) (string, bool, error) {
	for state.budgetLeft() {
		page, meta, err := p.client.Events(ctx, github.EventsRequest{
			Repository: repo,
			Number:     number,
			NextURL:    requestURL,
			ETag:       state.etags[requestURL],
		})
		state.pages++
		if err != nil {
			return requestURL, false, err
		}
		state.observe(meta)
		state.rememberETag(requestURL, meta.ETag)
		if err := p.admitEvents(ctx, state, repo, number, page.Items); err != nil {
			return requestURL, false, err
		}
		if meta.NextURL == "" {
			return requestURL, true, nil
		}
		state.cursor.NextURL = meta.NextURL
		requestURL = meta.NextURL
	}
	return requestURL, false, nil
}

func (p *Poller) labelledIssueNumbers(ctx context.Context, state *scanState, repo github.Repository) ([]int64, error) {
	var numbers []int64
	requestURL := p.labelledURL(repo)
	for state.budgetLeft() {
		page, meta, err := p.client.Issues(ctx, github.IssuesRequest{
			Repository: repo,
			NextURL:    requestURL,
			ETag:       state.etags[requestURL],
			Labels:     []string{"daltoki"},
		})
		state.pages++
		if err != nil {
			return nil, err
		}
		state.observe(meta)
		state.rememberETag(requestURL, meta.ETag)
		if meta.NotModified && len(numbers) == 0 {
			return state.cursor.IssueWorklist, nil
		}
		for _, item := range page.Items {
			if item.PullRequest == nil && item.State == "open" {
				numbers = append(numbers, item.Number)
			}
		}
		if meta.NextURL == "" {
			state.cursor.IssueWorklist = numbers
			return numbers, nil
		}
		requestURL = meta.NextURL
	}
	return numbers, nil
}

func (p *Poller) labelledURL(repo github.Repository) string {
	params := url.Values{}
	params.Set("state", "all")
	params.Set("labels", "daltoki")
	params.Set("per_page", "100")
	return strings.TrimRight(p.config.BaseURL, "/") + "/repos/" + repo.Owner + "/" + repo.Name + "/issues?" + params.Encode()
}

func (p *Poller) admitIssues(ctx context.Context, state *scanState, repo github.Repository, items []github.IssueItem) error {
	for _, item := range items {
		if item.PullRequest != nil {
			continue
		}
		obs := Observation{
			Kind:        "issue",
			Repository:  repo,
			ID:          strconv.FormatInt(item.ID, 10),
			IssueNumber: item.Number,
			Actor:       item.User.Login,
			ActorType:   item.User.Type,
			State:       item.State,
			Title:       item.Title,
			Body:        item.Body,
			URL:         item.HTMLURL,
			CreatedAt:   item.CreatedAt,
			UpdatedAt:   item.UpdatedAt,
		}
		for _, label := range item.Labels {
			obs.Labels = append(obs.Labels, label.Name)
		}
		if err := p.admitOne(ctx, state, obs, item.ID, item.UpdatedAt); err != nil {
			return err
		}
	}
	return nil
}

func (p *Poller) admitComments(ctx context.Context, state *scanState, repo github.Repository, items []github.Comment) error {
	for _, item := range items {
		obs := Observation{
			Kind:       "comment",
			Repository: repo,
			ID:         strconv.FormatInt(item.ID, 10),
			Actor:      item.User.Login,
			ActorType:  item.User.Type,
			Body:       item.Body,
			URL:        item.HTMLURL,
			CreatedAt:  item.CreatedAt,
			UpdatedAt:  item.UpdatedAt,
		}
		if err := p.admitOne(ctx, state, obs, item.ID, item.UpdatedAt); err != nil {
			return err
		}
	}
	return nil
}

func (p *Poller) admitEvents(ctx context.Context, state *scanState, repo github.Repository, number int64, items []github.Event) error {
	for _, item := range items {
		var label string
		if item.Label != nil {
			label = item.Label.Name
		}
		stamp := item.UpdatedAt
		if stamp.IsZero() {
			stamp = item.CreatedAt
		}
		obs := Observation{
			Kind:        "event",
			Repository:  repo,
			ID:          strconv.FormatInt(item.ID, 10),
			IssueNumber: number,
			Actor:       item.Actor.Login,
			ActorType:   item.Actor.Type,
			Action:      item.Action,
			Labels:      []string{label},
			Ignored:     item.Action != "labeled" && item.Action != "unlabeled",
			CreatedAt:   item.CreatedAt,
			UpdatedAt:   stamp,
		}
		if err := p.admitOne(ctx, state, obs, item.ID, stamp); err != nil {
			return err
		}
	}
	return nil
}

func (p *Poller) admitOne(ctx context.Context, state *scanState, obs Observation, id int64, updated time.Time) error {
	if id != 0 && state.boundary[id] && !updated.After(state.cursor.Since) {
		state.seen[id] = true
		return nil
	}
	admitted, err := p.store.Admit(ctx, obs)
	if err != nil {
		return err
	}
	if admitted {
		state.admitted++
	}
	return state.note(id)
}

func (s *scanState) note(id int64) error {
	if id == 0 {
		return nil
	}
	s.seen[id] = true
	if s.boundary[id] {
		return nil
	}
	if len(s.cursor.BoundaryIDs) >= pollBoundaryLimit {
		return errBoundaryOverflow
	}
	s.boundary[id] = true
	s.cursor.BoundaryIDs = append(s.cursor.BoundaryIDs, id)
	return nil
}

func (p *Poller) Run(ctx context.Context, interval time.Duration) error {
	if interval <= 0 {
		interval = p.config.Interval
	}
	if interval <= 0 {
		interval = reconciliationWait
	}
	for {
		result, err := p.Pass(ctx)
		delay := interval
		if err != nil {
			p.logger.Error("daltoki: polling pass failed", "error", err)
		} else {
			if result.Complete {
				p.logger.Debug("daltoki: polling pass complete", "admitted", result.Admitted)
			}
			if result.Wait > delay {
				delay = result.Wait
			}
		}
		timer := time.NewTimer(delay)
		select {
		case <-ctx.Done():
			timer.Stop()
			return ctx.Err()
		case <-timer.C:
		}
	}
}
