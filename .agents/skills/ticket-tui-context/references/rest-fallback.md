# Calling Azure DevOps directly

Use this only when no `ticket-tui` subcommand covers what you need — deleting a
work item, running your own WIQL, an endpoint ticket-tui does not implement.
For reading and for state, assignee, priority, iteration, area, title, tags,
description, comments, and creation, [cli.md](cli.md) is faster, carries the
revision guard, and keeps the local database and any running TUI in step.

## Contents

- [The MSA pass-through header](#the-msa-pass-through-header)
- [The token](#the-token)
- [A working request](#a-working-request)
- [Writes, if you really must](#writes-if-you-really-must)

## The MSA pass-through header

**This organization is backed by a Microsoft personal account.** Every request
must carry:

```
X-VSS-ForceMsaPassThrough: true
```

Without it Azure DevOps answers `302` with a redirect to a sign-in page instead
of the resource, whatever the token says. That reads like a wrong URL or an
expired login and is neither. Measured against
`https://dev.azure.com/jacobragsdale/_apis/wit/workitems/627?api-version=7.1`
with a valid bearer token: `200` with the header, `302` without it.

This is why `az boards` and generic REST snippets fail here and `ticket-tui`
does not — the client sets the header on every request.

## The token

```console
az account get-access-token --resource 499b84ac-1321-427f-aa17-267ca6975798 \
  --query accessToken -o tsv
```

`499b84ac-1321-427f-aa17-267ca6975798` is the Azure DevOps resource id; a token
for any other resource is refused. Tokens last about an hour. ticket-tui mints
one per sync and retries once with a fresh token when a request is refused.

`AZURE_DEVOPS_EXT_PAT`, when set, takes precedence in ticket-tui and switches to
Basic authentication with an empty username — `Authorization: Basic
base64(":$PAT")` — for environments without the Azure CLI.

A `401` or a `302` both mean rejected credentials: run `az login`, or add the
header above.

## A working request

```console
TOKEN=$(az account get-access-token \
  --resource 499b84ac-1321-427f-aa17-267ca6975798 --query accessToken -o tsv)

curl -sS \
  -H "Authorization: Bearer $TOKEN" \
  -H "X-VSS-ForceMsaPassThrough: true" \
  -H "Accept: application/json" \
  "https://dev.azure.com/jacobragsdale/_apis/wit/workitems/627?\$expand=relations&api-version=7.1"
```

Shapes ticket-tui uses, for reference:

| Purpose | Method and URL |
|---|---|
| Read work items | `GET {org}/_apis/wit/workitems?ids=1,2&$expand=relations&api-version=7.1` (200 ids per batch) |
| Run WIQL | `POST {org}/{project}/_apis/wit/wiql?timePrecision=true&api-version=7.1` |
| Change fields | `PATCH {org}/_apis/wit/workitems/{id}?api-version=7.1` |
| Create | `POST {org}/{project}/_apis/wit/workitems/$Issue?$expand=relations&api-version=7.1` |
| Comments | `{org}/_apis/wit/workItems/{id}/comments?api-version=7.1-preview.4` — still preview on every 7.x |
| Repositories | `GET {org}/{project}/_apis/git/repositories?api-version=7.1` |
| Branches | `GET {org}/{project}/_apis/git/repositories/{repo}/refs?filter=heads/&api-version=7.1` |
| Pull requests | `GET {org}/{project}/_apis/git/pullrequests?searchCriteria.status=active&api-version=7.1` |
| One pull request | `GET`/`PATCH {org}/{project}/_apis/git/repositories/{repo}/pullrequests/{id}?api-version=7.1` |
| A vote | `PUT {org}/{project}/_apis/git/repositories/{repo}/pullrequests/{id}/reviewers/{reviewerId}?api-version=7.1` |
| PR threads | `{org}/{project}/_apis/git/repositories/{repo}/pullrequests/{id}/threads?api-version=7.1` |
| PR work items | `GET .../pullrequests/{id}/workitems?api-version=7.1` |
| Build definitions | `GET {org}/{project}/_apis/build/definitions?includeLatestBuilds=true&api-version=7.1` |
| Builds | `GET {org}/{project}/_apis/build/builds?api-version=7.1` |
| One build | `GET`/`PATCH {org}/{project}/_apis/build/builds/{id}?api-version=7.1` |
| Start one | `POST {org}/{project}/_apis/pipelines/{definitionId}/runs?api-version=7.1` |
| Timeline | `GET {org}/{project}/_apis/build/builds/{id}/timeline?api-version=7.1` |
| A log | `GET {org}/{project}/_apis/build/builds/{id}/logs/{logId}?api-version=7.1` — plain text, not JSON |
| Approvals | `GET {org}/{project}/_apis/pipelines/approvals?api-version=7.1-preview.1`, `PATCH` the same to answer |
| Who am I | `GET https://dev.azure.com/{organization}/_apis/connectionData?api-version=7.1` |

`{org}` is `https://dev.azure.com/<organization>`; `{repo}` is the repository's
GUID, which `ticket-tui repos list --json` prints as `id`.

Two things about the pipeline endpoints that cost an hour to find out:

- A timeline record's `log.id` is `0` for a node that wrote nothing — the
  endpoint exists and answers empty. And the log an agent wants for a job is
  usually on the **Phase** record above it, not on the Job record itself.
- The pull-request list does not always carry `_links.web.href`. The browser URL
  is `{org}/{project}/_git/{repo-name}/pullrequest/{id}`.

## Writes, if you really must

`PATCH` and the creating `POST` take a JSON Patch document under
`Content-Type: application/json-patch+json`. Lead it with a revision test, the
way ticket-tui does, so a work item somebody else moved on is refused rather
than overwritten:

```json
[
  {"op": "test", "path": "/rev", "value": 4},
  {"op": "add", "path": "/fields/System.State", "value": "Doing"}
]
```

A parent link is an operation, not a field:

```json
{"op": "add", "path": "/relations/-", "value": {
  "rel": "System.LinkTypes.Hierarchy-Reverse",
  "url": "https://dev.azure.com/jacobragsdale/_apis/wit/workItems/624"
}}
```

Note what you lose by going around the CLI: the local database is not updated,
so `ticket-tui list` and a running TUI keep showing the old value until the next
pull. Run `ticket-tui sync` afterwards.
