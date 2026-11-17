package github

import "time"

type Repository struct {
	Owner string
	Name  string
}

type Token struct {
	Value     string
	ExpiresAt time.Time
}

type CredentialRequest struct {
	Repository Repository
	Workflow   bool
}

type ResponseMeta struct {
	Status       int
	Limit        int
	Remaining    int
	Used         int
	Reset        time.Time
	Resource     string
	RetryAfter   time.Time
	PollInterval time.Duration
	ETag         string
	NextURL      string
	NotModified  bool
}

type IssueSearch struct {
	Query   string
	Page    int
	PerPage int
}

type Issue struct {
	ID          int64
	Number      int64
	Repository  string
	Title       string
	Body        string
	URL         string
	PullRequest bool
	Labels      []string
	Actor       string
	ActorType   string
	CreatedAt   time.Time
	UpdatedAt   time.Time
}

type SearchResult struct {
	Items      []Issue
	TotalCount int
	Incomplete bool
}

type CreateIssue struct {
	Owner  string
	Repo   string
	Title  string
	Body   string
	Labels []string
}

type IssueReaction struct {
	Owner   string
	Repo    string
	Number  int64
	Content string
}

type RepositoryInfo struct {
	ID            int64
	FullName      string
	DefaultBranch string
	CloneURL      string
}

type Permission struct {
	Login      string
	UserType   string
	Permission string
	Push       bool
}

type IssuesRequest struct {
	Repository Repository
	NextURL    string
	ETag       string
	Since      time.Time
	Labels     []string
}

type IssueItem struct {
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

type ghLabel struct {
	Name string `json:"name"`
}

type ghPullRef struct {
	URL string `json:"url"`
}

type ghActor struct {
	Login string `json:"login"`
	Type  string `json:"type"`
}

type IssuesPage struct {
	Items []IssueItem
}

type CommentsRequest struct {
	Repository Repository
	NextURL    string
	ETag       string
	Since      time.Time
	Number     int64
}

type Comment struct {
	ID        int64     `json:"id"`
	Body      string    `json:"body"`
	HTMLURL   string    `json:"html_url"`
	User      ghActor   `json:"user"`
	CreatedAt time.Time `json:"created_at"`
	UpdatedAt time.Time `json:"updated_at"`
}

type CommentsPage struct {
	Items []Comment
}

type EventsRequest struct {
	Repository Repository
	NextURL    string
	ETag       string
	Number     int64
}

type Event struct {
	ID        int64     `json:"id"`
	Action    string    `json:"event"`
	Actor     ghActor   `json:"actor"`
	Label     *ghLabel  `json:"label"`
	CreatedAt time.Time `json:"created_at"`
	UpdatedAt time.Time `json:"updated_at"`
}

type EventsPage struct {
	Items []Event
}

type CreateCommentRequest struct {
	Repository Repository
	Number     int64
	Body       string
}

type ChangeLabelRequest struct {
	Repository Repository
	Number     int64
	Label      string
	Add        bool
}

type LabelReceipt struct {
	ID  int64
	URL string
}

type CreatePullRequestRequest struct {
	Repository Repository
	Title      string
	Body       string
	Head       string
	Base       string
	Draft      bool
}

type PullRequest struct {
	ID        int64     `json:"id"`
	Number    int64     `json:"number"`
	HTMLURL   string    `json:"html_url"`
	Title     string    `json:"title"`
	Body      string    `json:"body"`
	State     string    `json:"state"`
	Draft     bool      `json:"draft"`
	Head      ghRef     `json:"head"`
	Base      ghRef     `json:"base"`
	User      ghActor   `json:"user"`
	CreatedAt time.Time `json:"created_at"`
	UpdatedAt time.Time `json:"updated_at"`
}

type ghRef struct {
	Ref string `json:"ref"`
}

type PullRequestsRequest struct {
	Repository Repository
	NextURL    string
	ETag       string
	Head       string
}

type PullRequestsPage struct {
	Items []PullRequest
}
