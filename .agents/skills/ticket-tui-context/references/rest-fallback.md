# Calling Azure DevOps directly

Use this only when no `ticket-tui` subcommand covers what you need — deleting a
work item, running your own WIQL, an endpoint ticket-tui does not implement.
For reading and for state, assignee, priority, iteration, area, title, tags,
description, comments, and creation, [cli.md](cli.md) is faster, carries the
revision guard, and keeps the local database and any running TUI in step.

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

`{org}` is `https://dev.azure.com/<organization>`.

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
