# Shared GitHub client

The client lives in `internal/dalgona/github`. It serves exactly two consumers. Daltoki uses the `bot` policy. The failure reporter uses the `reporter` policy. No other policy value is valid.

Production `BaseURL` is fixed to `https://api.github.com`. Loopback HTTP is allowed only in explicit test construction. The client MUST send API version `2026-03-10` and accept `application/vnd.github+json`.

## Credentials

`Repository` holds `Owner` and `Name`. `Token` holds `Value` and `ExpiresAt`. `CredentialRequest` holds one `Repository` and a `Workflow` flag. `Credential` is the consumer interface with `Token(ctx, request)`.

`InstallationCredential` signs a JWT and exchanges it for an installation token. `InstallationConfig` holds `AppID`, `PrivateKeyFile`, and `Repositories`. The key file MUST hold a PKCS#1 or PKCS#8 RSA key of at least 2048 bits. Any other key MUST fail with `github: invalid rsa private key`. The loader MUST read at most 65536 key bytes. It MUST refuse symlinks and nonregular files. On Unix the file MUST be owned by the current user with mode `0077` clear. On Windows the file MUST be private to the current user, SYSTEM, and Administrators. A permission failure MUST fail closed with `github: private key file must be private to the current user`. The loader MUST open the key with no-follow semantics and validate the opened descriptor. Key bytes MUST NOT enter errors, journals, model inputs, or worktrees.

JWT claims MUST use `iat` of now minus 60 seconds and `exp` of now plus 540 seconds. The issuer MUST be the decimal app id. The client MUST discover the installation through the API. It MUST request a token scoped to one configured repository and the three configured permission families. It MUST never request workflow permission through app credentials. Tokens stay in memory only. The client MUST treat token values as opaque. It MUST refresh 120 seconds before expiry.

`UserCredential` resolves only the dedicated token source of its caller. It MUST NOT read `GH_TOKEN`, invoke a CLI, or fall back to installation credentials. The failure reporter supplies its selected source through this credential.

## Policies

`NewClient` takes `ClientConfig` with `BaseURL`, `Repositories`, and `Policy`. Policy is exactly `bot` or `reporter`.

The `bot` policy MUST use a 30-second timeout and an 8 MiB response cap. The `reporter` policy MUST use a 10-second timeout and a 1 MiB cap. The reporter policy MUST follow no redirects and MUST NOT retry automatically. The client MUST drain and close bodies within the response cap. Bot redirects are limited to three same-origin hops with allowlist revalidation. The client MUST never forward authorization cross-origin.

## Responses and rate limits

`ResponseMeta` carries `Status`, `Limit`, `Remaining`, `Used`, `Reset`, `Resource`, `RetryAfter`, `PollInterval`, `ETag`, `NextURL`, and `NotModified`. Every operation MUST return its `ResponseMeta`.

Safe bot reads MUST retry transport failures and 502, 503, and 504 responses at 1, 2, and 4 seconds, at most three retries. One 401 token refresh is allowed for safe reads only. The client MUST honor the maximum of `Retry-After`, reset plus one second, and secondary-limit delays of 60, 120, and 240 seconds. Exhaustion MUST fail with `github: rate limit retry budget exceeded`. Longer waits belong in the service, not in a job worker. Bot mutations MUST be spaced at least one second apart per credential. Requests serialize per credential.

The client MUST never retry a mutation after an ambiguous transport failure, a 5xx response, or an undecodable response. Ordinary 403, 404, 410, and 422 responses are terminal for the bot policy. Recognized rate-limit responses follow the bot rate-wait rules. The reporter policy returns every response without refreshing, retrying, or rotating credentials.

## Errors

`APIError` holds `Status`, `RequestID`, `Message`, and `RetryAt`. Its text MUST read `github: <operation>: status <status>`. `AmbiguousWriteError` holds `Operation` and `RequestID`. Its text MUST read `github: <operation>: publication outcome unknown`. An ambiguous outcome MUST NOT be retried automatically. Empty required input MUST fail with `github: invalid request`. A repository outside the allowlist MUST fail with `github: repository is not allowlisted`. Empty pages and 304 responses are successful. Errors MUST NOT carry credentials.

## Reporter operations

Shared reporter operations are `SearchIssues` and `CreateIssue` with the exact signatures of the Daltoki step. Its search query MUST be scoped to its configured repository before dispatch. `ReactIssue` takes `Owner`, `Repo`, `Number`, and `Content`. Reporter calls MUST validate repository allowlisting before dispatch. Reporter calls use content `eyes`.

## Bot operations

Bot operations return their named typed result with `ResponseMeta` and an error. Repository reads return `RepositoryInfo` with `ID`, `FullName`, `DefaultBranch`, and `CloneURL`. Permission reads return `Login`, `UserType`, `Permission`, and `Push`. Page requests carry the repository, the next URL, and the ETag. Issues and comments carry `Since`. Comments and events carry `Number`. Pull-request listings carry `Head`. Page results carry their typed item slices. Mutation requests carry the repository and, where relevant, the issue number. Label changes carry `Label` and `Add`. Pull-request creation carries title, body, head, base, and `Draft`. Receipts carry the GitHub numeric id and URL where supplied.
