# GitHub v2 Connector

GitHub repositories, branches, commits, file changes, and pull requests via GraphQL + REST APIs.

Replaces the v1 GitHub connector with bulk GraphQL queries for PRs (embedded commits, reviews, comments, review threads) and per-commit file changes via REST.

## Prerequisites

1. Create a GitHub Personal Access Token (classic) with scopes: `repo`, `read:org`, `read:user`
2. Or use a GitHub App installation token with repository read access

## K8s Secret

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: insight-github-v2-main
  labels:
    app.kubernetes.io/part-of: insight
  annotations:
    insight.cyberfabric.com/connector: github-v2
    insight.cyberfabric.com/source-id: github-v2-main
type: Opaque
stringData:
  github_token: "ghp_CHANGE_ME"
  github_organizations: '["myorg"]'
  github_start_date: "2024-01-01"
  github_skip_archived: "true"
  github_skip_forks: "true"
```

### Fields

| Field | Required | Description |
|-------|----------|-------------|
| `github_token` | Yes | GitHub PAT or App installation token |
| `github_organizations` | Yes | JSON array of org logins (e.g. `["myorg"]`) |
| `github_start_date` | No | Initial collection start date, `YYYY-MM-DD` |
| `github_skip_archived` | No | Skip archived repos (default: `true`) |
| `github_skip_forks` | No | Skip forked repos (default: `true`) |
| `github_pr_page_size` | No | PRs per GraphQL page, 10-100 (default: `25`) |

### Automatically injected

| Field | Source |
|-------|--------|
| `insight_tenant_id` | `tenant_id` from tenant YAML |
| `insight_source_id` | `insight.cyberfabric.com/source-id` annotation |

### Local development

```bash
cp src/ingestion/secrets/connectors/github-v2.yaml.example src/ingestion/secrets/connectors/github-v2.yaml
# Fill in real values, then apply:
kubectl apply -f src/ingestion/secrets/connectors/github-v2.yaml
```

## Streams

| Stream | API | Sync Mode | Parent |
|--------|-----|-----------|--------|
| `repositories` | REST | Full refresh | — |
| `branches` | REST | Full refresh | repositories |
| `commits` | GraphQL | Incremental (per branch) | branches |
| `file_changes` | REST | Incremental (per commit) | commits |
| `pull_requests` | GraphQL | Incremental (per repo) | repositories |
| `pull_request_commits` | GraphQL | Incremental (per PR) | pull_requests |
| `pull_request_comments` | GraphQL | Incremental (per PR) | pull_requests |
| `pull_request_reviews` | GraphQL | Incremental (per PR) | pull_requests |
| `pull_request_review_comments` | GraphQL | Incremental (per PR) | pull_requests |

## Silver Targets

- `class_git_repositories`
- `class_git_repository_branches`
- `class_git_commits`
- `class_git_file_changes`
- `class_git_pull_requests`
- `class_git_pull_requests_commits`
- `class_git_pull_requests_comments`
- `class_git_pull_requests_reviewers`

## Known Limitations

- GitHub GraphQL API has a 5,000 points/hour rate limit per token. Large orgs may need multiple tokens or lower `github_pr_page_size`.
- PR child data (commits, reviews, comments) is embedded in bulk PR queries to reduce API calls. Overflow pages are fetched separately.
- File changes use REST (`GET /repos/{owner}/{repo}/commits/{sha}`) — one call per non-merge commit.
