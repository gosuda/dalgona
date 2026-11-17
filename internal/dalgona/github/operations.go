package github

import (
	"context"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"sort"
	"strconv"
	"strings"
	"time"
)

func repoPath(repo Repository) string {
	return "/repos/" + repo.Owner + "/" + repo.Name
}

func repoQuery(params url.Values, repo Repository) string {
	params.Set("per_page", "100")
	return repoPath(repo) + "?" + params.Encode()
}

type listQuery struct {
	repository Repository
	nextURL    string
	etag       string
	fallback   string
	operation  string
}

func (c *Client) fetchList(ctx context.Context, query listQuery) (roundResult, error) {
	if err := c.checkRepository(query.repository); err != nil {
		return roundResult{}, err
	}
	rawURL, err := c.resolveURL(query.nextURL, query.fallback)
	if err != nil {
		return roundResult{}, err
	}
	headers := map[string]string{}
	if query.etag != "" {
		headers["If-None-Match"] = query.etag
	}
	return c.do(ctx, requestSpec{
		method:    http.MethodGet,
		url:       rawURL,
		repo:      query.repository,
		headers:   headers,
		operation: query.operation,
	})
}

func listBody(result roundResult) ([]byte, bool) {
	if result.meta.NotModified || len(result.data) == 0 {
		return nil, false
	}
	return result.data, true
}

func setSinceParam(params url.Values, since time.Time) {
	if !since.IsZero() {
		params.Set("since", since.UTC().Format(time.RFC3339))
	}
}

type commentPayload struct {
	Body string `json:"body"`
}

type labelPayload struct {
	Labels []string `json:"labels"`
}

type pullRequestPayload struct {
	Title string `json:"title"`
	Body  string `json:"body"`
	Head  string `json:"head"`
	Base  string `json:"base"`
	Draft bool   `json:"draft"`
}

type issuePayload struct {
	Title  string   `json:"title"`
	Body   string   `json:"body"`
	Labels []string `json:"labels,omitempty"`
}

type reactionPayload struct {
	Content string `json:"content"`
}

func (c *Client) Repository(ctx context.Context, repo Repository) (RepositoryInfo, ResponseMeta, error) {
	var info RepositoryInfo
	if err := c.checkRepository(repo); err != nil {
		return info, ResponseMeta{}, err
	}
	result, err := c.do(ctx, requestSpec{
		method:    http.MethodGet,
		url:       c.apiURL(repoPath(repo)),
		repo:      repo,
		operation: "repository",
	})
	if err != nil {
		return info, result.meta, err
	}
	data := result.data
	var decoded struct {
		ID            int64  `json:"id"`
		FullName      string `json:"full_name"`
		DefaultBranch string `json:"default_branch"`
		CloneURL      string `json:"clone_url"`
	}
	if err := json.Unmarshal(data, &decoded); err != nil {
		return info, result.meta, fmt.Errorf("github: repository: decode response: %w", err)
	}
	info = RepositoryInfo{
		ID:            decoded.ID,
		FullName:      decoded.FullName,
		DefaultBranch: decoded.DefaultBranch,
		CloneURL:      decoded.CloneURL,
	}
	return info, result.meta, nil
}

func (c *Client) Permission(ctx context.Context, repo Repository, login string) (Permission, ResponseMeta, error) {
	var perm Permission
	if err := c.checkRepository(repo); err != nil {
		return perm, ResponseMeta{}, err
	}
	if login == "" {
		return perm, ResponseMeta{}, errors.New("github: invalid request")
	}
	result, err := c.do(ctx, requestSpec{
		method:    http.MethodGet,
		url:       c.apiURL(repoPath(repo) + "/collaborators/" + url.PathEscape(login) + "/permission"),
		repo:      repo,
		operation: "permission",
	})
	if err != nil {
		return perm, result.meta, err
	}
	data := result.data
	var decoded struct {
		User struct {
			Login string `json:"login"`
			Type  string `json:"type"`
		} `json:"user"`
		Permission string `json:"permission"`
		RoleName   string `json:"role_name"`
	}
	if err := json.Unmarshal(data, &decoded); err != nil {
		return perm, result.meta, fmt.Errorf("github: permission: decode response: %w", err)
	}
	perm = Permission{
		Login:      decoded.User.Login,
		UserType:   decoded.User.Type,
		Permission: decoded.Permission,
		Push:       decoded.Permission == "admin" || decoded.Permission == "write" || decoded.Permission == "maintain",
	}
	return perm, result.meta, nil
}

func (c *Client) Issues(ctx context.Context, request IssuesRequest) (IssuesPage, ResponseMeta, error) {
	var page IssuesPage
	params := url.Values{}
	params.Set("state", "all")
	if len(request.Labels) > 0 {
		params.Set("labels", strings.Join(request.Labels, ","))
	}
	setSinceParam(params, request.Since)
	fallback := c.apiURL(repoQuery(params, request.Repository))
	result, err := c.fetchList(ctx, listQuery{
		repository: request.Repository,
		nextURL:    request.NextURL,
		etag:       request.ETag,
		fallback:   fallback,
		operation:  "issues",
	})
	if err != nil {
		return page, result.meta, err
	}
	data, ok := listBody(result)
	if !ok {
		return page, result.meta, nil
	}
	var decoded []struct {
		ID          int64      `json:"id"`
		Number      int64      `json:"number"`
		Title       string     `json:"title"`
		Body        string     `json:"body"`
		State       string     `json:"state"`
		HTMLURL     string     `json:"html_url"`
		PullRequest *ghPullRef `json:"pull_request"`
		Labels      []ghLabel  `json:"labels"`
		User        ghActor    `json:"user"`
		CreatedAt   time.Time  `json:"created_at"`
		UpdatedAt   time.Time  `json:"updated_at"`
	}
	if err := json.Unmarshal(data, &decoded); err != nil {
		return page, result.meta, fmt.Errorf("github: issues: decode response: %w", err)
	}
	page.Items = make([]IssueItem, 0, len(decoded))
	for _, item := range decoded {
		page.Items = append(page.Items, IssueItem{
			ID:          item.ID,
			Number:      item.Number,
			Title:       item.Title,
			Body:        item.Body,
			State:       item.State,
			HTMLURL:     item.HTMLURL,
			PullRequest: item.PullRequest,
			Labels:      item.Labels,
			User:        item.User,
			CreatedAt:   item.CreatedAt,
			UpdatedAt:   item.UpdatedAt,
		})
	}
	return page, result.meta, nil
}

func (c *Client) Comments(ctx context.Context, request CommentsRequest) (CommentsPage, ResponseMeta, error) {
	var page CommentsPage
	var fallback string
	if request.NextURL == "" {
		params := url.Values{}
		params.Set("per_page", "100")
		setSinceParam(params, request.Since)
		path := repoPath(request.Repository) + "/issues/comments"
		if request.Number != 0 {
			path = repoPath(request.Repository) + "/issues/" + strconv.FormatInt(request.Number, 10) + "/comments"
		}
		fallback = c.apiURL(path + "?" + params.Encode())
	}
	result, err := c.fetchList(ctx, listQuery{
		repository: request.Repository,
		nextURL:    request.NextURL,
		etag:       request.ETag,
		fallback:   fallback,
		operation:  "comments",
	})
	if err != nil {
		return page, result.meta, err
	}
	data, ok := listBody(result)
	if !ok {
		return page, result.meta, nil
	}
	var decoded []Comment
	if err := json.Unmarshal(data, &decoded); err != nil {
		return page, result.meta, fmt.Errorf("github: comments: decode response: %w", err)
	}
	page.Items = decoded
	return page, result.meta, nil
}

func (c *Client) Events(ctx context.Context, request EventsRequest) (EventsPage, ResponseMeta, error) {
	var page EventsPage
	var fallback string
	if request.NextURL == "" {
		if request.Number == 0 {
			return page, ResponseMeta{}, errors.New("github: invalid request")
		}
		fallback = c.apiURL(repoPath(request.Repository) + "/issues/" + strconv.FormatInt(request.Number, 10) + "/events?per_page=100")
	}
	result, err := c.fetchList(ctx, listQuery{
		repository: request.Repository,
		nextURL:    request.NextURL,
		etag:       request.ETag,
		fallback:   fallback,
		operation:  "events",
	})
	if err != nil {
		return page, result.meta, err
	}
	data, ok := listBody(result)
	if !ok {
		return page, result.meta, nil
	}
	var decoded []Event
	if err := json.Unmarshal(data, &decoded); err != nil {
		return page, result.meta, fmt.Errorf("github: events: decode response: %w", err)
	}
	page.Items = decoded
	return page, result.meta, nil
}

func (c *Client) CreateComment(ctx context.Context, request CreateCommentRequest) (Comment, ResponseMeta, error) {
	var comment Comment
	if err := c.checkRepository(request.Repository); err != nil {
		return comment, ResponseMeta{}, err
	}
	if request.Number == 0 || request.Body == "" {
		return comment, ResponseMeta{}, errors.New("github: invalid request")
	}
	payload, err := json.Marshal(commentPayload{Body: request.Body})
	if err != nil {
		return comment, ResponseMeta{}, err
	}
	result, err := c.do(ctx, requestSpec{
		method:    http.MethodPost,
		url:       c.apiURL(repoPath(request.Repository) + "/issues/" + strconv.FormatInt(request.Number, 10) + "/comments"),
		body:      payload,
		mutate:    true,
		repo:      request.Repository,
		operation: "create comment",
	})
	if err != nil {
		return comment, result.meta, err
	}
	if result.meta.Status == http.StatusNoContent || len(result.data) == 0 {
		return comment, result.meta, nil
	}
	if err := json.Unmarshal(result.data, &comment); err != nil {
		return comment, result.meta, &AmbiguousWriteError{Operation: "create comment", RequestID: result.requestID}
	}
	return comment, result.meta, nil
}

func parseLabelReceipt(data []byte) LabelReceipt {
	var decoded []struct {
		ID  int64  `json:"id"`
		URL string `json:"url"`
	}
	if err := json.Unmarshal(data, &decoded); err == nil && len(decoded) > 0 {
		return LabelReceipt{ID: decoded[0].ID, URL: decoded[0].URL}
	}
	return LabelReceipt{}
}

func (c *Client) ChangeLabel(ctx context.Context, request ChangeLabelRequest) (LabelReceipt, ResponseMeta, error) {
	var receipt LabelReceipt
	if err := c.checkRepository(request.Repository); err != nil {
		return receipt, ResponseMeta{}, err
	}
	if request.Number == 0 || request.Label == "" {
		return receipt, ResponseMeta{}, errors.New("github: invalid request")
	}
	payload, err := json.Marshal(labelPayload{Labels: []string{request.Label}})
	if err != nil {
		return receipt, ResponseMeta{}, err
	}
	method := http.MethodPost
	if !request.Add {
		method = http.MethodDelete
	}
	endpoint := c.apiURL(repoPath(request.Repository) + "/issues/" + strconv.FormatInt(request.Number, 10) + "/labels")
	result, err := c.do(ctx, requestSpec{
		method:    method,
		url:       endpoint,
		body:      payload,
		mutate:    true,
		repo:      request.Repository,
		operation: "change label",
	})
	if err != nil {
		return receipt, result.meta, err
	}
	if len(result.data) > 0 {
		receipt = parseLabelReceipt(result.data)
	}
	return receipt, result.meta, nil
}

func (c *Client) CreatePullRequest(ctx context.Context, request CreatePullRequestRequest) (PullRequest, ResponseMeta, error) {
	var pr PullRequest
	if err := c.checkRepository(request.Repository); err != nil {
		return pr, ResponseMeta{}, err
	}
	if request.Title == "" || request.Head == "" || request.Base == "" {
		return pr, ResponseMeta{}, errors.New("github: invalid request")
	}
	payload, err := json.Marshal(pullRequestPayload{
		Title: request.Title,
		Body:  request.Body,
		Head:  request.Head,
		Base:  request.Base,
		Draft: request.Draft,
	})
	if err != nil {
		return pr, ResponseMeta{}, err
	}
	result, err := c.do(ctx, requestSpec{
		method:    http.MethodPost,
		url:       c.apiURL(repoPath(request.Repository) + "/pulls"),
		body:      payload,
		mutate:    true,
		repo:      request.Repository,
		operation: "create pull request",
	})
	if err != nil {
		return pr, result.meta, err
	}
	if err := json.Unmarshal(result.data, &pr); err != nil {
		return pr, result.meta, &AmbiguousWriteError{Operation: "create pull request", RequestID: result.requestID}
	}
	return pr, result.meta, nil
}

func (c *Client) PullRequests(ctx context.Context, request PullRequestsRequest) (PullRequestsPage, ResponseMeta, error) {
	var page PullRequestsPage
	var fallback string
	if request.NextURL == "" {
		params := url.Values{}
		params.Set("state", "all")
		params.Set("per_page", "100")
		if request.Head != "" {
			params.Set("head", request.Repository.Owner+":"+request.Head)
		}
		fallback = c.apiURL(repoPath(request.Repository) + "/pulls?" + params.Encode())
	}
	result, err := c.fetchList(ctx, listQuery{
		repository: request.Repository,
		nextURL:    request.NextURL,
		etag:       request.ETag,
		fallback:   fallback,
		operation:  "pull requests",
	})
	if err != nil {
		return page, result.meta, err
	}
	data, ok := listBody(result)
	if !ok {
		return page, result.meta, nil
	}
	var decoded []PullRequest
	if err := json.Unmarshal(data, &decoded); err != nil {
		return page, result.meta, fmt.Errorf("github: pull requests: decode response: %w", err)
	}
	page.Items = decoded
	return page, result.meta, nil
}

func (c *Client) firstRepository() (Repository, error) {
	if len(c.repositories) == 0 {
		return Repository{}, errors.New("github: repository is not allowlisted")
	}
	repos := make([]Repository, 0, len(c.repositories))
	for repo := range c.repositories {
		repos = append(repos, repo)
	}
	sort.Slice(repos, func(i, j int) bool {
		if repos[i].Owner != repos[j].Owner {
			return repos[i].Owner < repos[j].Owner
		}
		return repos[i].Name < repos[j].Name
	})
	return repos[0], nil
}

func scopedSearchQuery(repo Repository, raw string) (string, error) {
	trimmed := strings.TrimSpace(raw)
	if trimmed == "" {
		return "", errors.New("github: invalid request")
	}
	return "repo:" + repo.Owner + "/" + repo.Name + " " + trimmed, nil
}

func searchParams(query IssueSearch, scoped string) url.Values {
	params := url.Values{}
	params.Set("q", scoped)
	if query.Page > 0 {
		params.Set("page", strconv.Itoa(query.Page))
	}
	params.Set("per_page", strconv.Itoa(clampPerPage(query.PerPage, 30)))
	return params
}

func clampPerPage(perPage, fallback int) int {
	if perPage <= 0 {
		return fallback
	}
	return min(perPage, 100)
}

type searchItemRepository struct {
	FullName string `json:"full_name"`
}

type searchItem struct {
	ID          int64                `json:"id"`
	Number      int64                `json:"number"`
	Title       string               `json:"title"`
	Body        string               `json:"body"`
	HTMLURL     string               `json:"html_url"`
	PullRequest *ghPullRef           `json:"pull_request"`
	Labels      []ghLabel            `json:"labels"`
	User        ghActor              `json:"user"`
	Repository  searchItemRepository `json:"repository"`
	CreatedAt   time.Time            `json:"created_at"`
	UpdatedAt   time.Time            `json:"updated_at"`
}

type searchResponse struct {
	TotalCount        int          `json:"total_count"`
	IncompleteResults bool         `json:"incomplete_results"`
	Items             []searchItem `json:"items"`
}

func toIssue(item searchItem) Issue {
	labels := make([]string, 0, len(item.Labels))
	for _, label := range item.Labels {
		labels = append(labels, label.Name)
	}
	return Issue{
		ID:          item.ID,
		Number:      item.Number,
		Repository:  item.Repository.FullName,
		Title:       item.Title,
		Body:        item.Body,
		URL:         item.HTMLURL,
		PullRequest: item.PullRequest != nil,
		Labels:      labels,
		Actor:       item.User.Login,
		ActorType:   item.User.Type,
		CreatedAt:   item.CreatedAt,
		UpdatedAt:   item.UpdatedAt,
	}
}

func (c *Client) SearchIssues(ctx context.Context, query IssueSearch) (SearchResult, ResponseMeta, error) {
	var result SearchResult
	repo, err := c.firstRepository()
	if err != nil {
		return result, ResponseMeta{}, err
	}
	scoped, err := scopedSearchQuery(repo, query.Query)
	if err != nil {
		return result, ResponseMeta{}, err
	}
	params := searchParams(query, scoped)
	res, err := c.do(ctx, requestSpec{
		method:    http.MethodGet,
		url:       c.apiURL("/search/issues?" + params.Encode()),
		repo:      repo,
		operation: "search issues",
	})
	if err != nil {
		return result, res.meta, err
	}
	var decoded searchResponse
	if err := json.Unmarshal(res.data, &decoded); err != nil {
		return result, res.meta, fmt.Errorf("github: search issues: decode response: %w", err)
	}
	result.TotalCount = decoded.TotalCount
	result.Incomplete = decoded.IncompleteResults
	result.Items = make([]Issue, 0, len(decoded.Items))
	for _, item := range decoded.Items {
		result.Items = append(result.Items, toIssue(item))
	}
	return result, res.meta, nil
}

func (c *Client) CreateIssue(ctx context.Context, input CreateIssue) (Issue, ResponseMeta, error) {
	var issue Issue
	repo := Repository{Owner: input.Owner, Name: input.Repo}
	if err := c.checkRepository(repo); err != nil {
		return issue, ResponseMeta{}, err
	}
	if input.Title == "" {
		return issue, ResponseMeta{}, errors.New("github: invalid request")
	}
	payload, err := json.Marshal(issuePayload{
		Title:  input.Title,
		Body:   input.Body,
		Labels: input.Labels,
	})
	if err != nil {
		return issue, ResponseMeta{}, err
	}
	result, err := c.do(ctx, requestSpec{
		method:    http.MethodPost,
		url:       c.apiURL(repoPath(repo) + "/issues"),
		body:      payload,
		mutate:    true,
		repo:      repo,
		operation: "create issue",
	})
	if err != nil {
		return issue, result.meta, err
	}
	var decoded createdIssue
	if err := json.Unmarshal(result.data, &decoded); err != nil {
		return issue, result.meta, &AmbiguousWriteError{Operation: "create issue", RequestID: result.requestID}
	}
	issue = toCreatedIssue(decoded, input.Owner+"/"+input.Repo)
	return issue, result.meta, nil
}

type createdIssue struct {
	ID          int64      `json:"id"`
	Number      int64      `json:"number"`
	Title       string     `json:"title"`
	Body        string     `json:"body"`
	HTMLURL     string     `json:"html_url"`
	PullRequest *ghPullRef `json:"pull_request"`
	Labels      []ghLabel  `json:"labels"`
	User        ghActor    `json:"user"`
	CreatedAt   time.Time  `json:"created_at"`
	UpdatedAt   time.Time  `json:"updated_at"`
}

func toCreatedIssue(decoded createdIssue, repository string) Issue {
	labels := make([]string, 0, len(decoded.Labels))
	for _, label := range decoded.Labels {
		labels = append(labels, label.Name)
	}
	return Issue{
		ID:          decoded.ID,
		Number:      decoded.Number,
		Repository:  repository,
		Title:       decoded.Title,
		Body:        decoded.Body,
		URL:         decoded.HTMLURL,
		PullRequest: decoded.PullRequest != nil,
		Labels:      labels,
		Actor:       decoded.User.Login,
		ActorType:   decoded.User.Type,
		CreatedAt:   decoded.CreatedAt,
		UpdatedAt:   decoded.UpdatedAt,
	}
}

var validReactionContents = map[string]bool{
	"+1": true, "-1": true, "laugh": true, "hooray": true,
	"confused": true, "heart": true, "rocket": true, "eyes": true,
}

func (c *Client) ReactIssue(ctx context.Context, input IssueReaction) (ResponseMeta, error) {
	repo := Repository{Owner: input.Owner, Name: input.Repo}
	if err := c.checkRepository(repo); err != nil {
		return ResponseMeta{}, err
	}
	if input.Number == 0 || !validReactionContents[input.Content] {
		return ResponseMeta{}, errors.New("github: invalid request")
	}
	payload, err := json.Marshal(reactionPayload{Content: input.Content})
	if err != nil {
		return ResponseMeta{}, err
	}
	result, err := c.do(ctx, requestSpec{
		method:    http.MethodPost,
		url:       c.apiURL(repoPath(repo) + "/issues/" + strconv.FormatInt(input.Number, 10) + "/reactions"),
		body:      payload,
		mutate:    true,
		repo:      repo,
		operation: "issue reaction",
	})
	return result.meta, err
}
