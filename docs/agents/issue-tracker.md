# Issue tracker: GitHub

Issues and PRDs that drive this repository live in the central
`LioRael/lenso` GitHub repository. Use the `gh` CLI with
`--repo LioRael/lenso` for issue operations.

Code pull requests live in `LioRael/lenso-organization-plugin`.

## Issue conventions

- **Create**: `gh issue create --repo LioRael/lenso --title "..." --body "..."`
- **Read**: `gh issue view <number> --repo LioRael/lenso --comments`
- **List**: `gh issue list --repo LioRael/lenso --state open`
- **Comment**: `gh issue comment <number> --repo LioRael/lenso --body "..."`
- **Label**: `gh issue edit <number> --repo LioRael/lenso --add-label "..."`
- **Claim**: `gh issue edit <number> --repo LioRael/lenso --add-assignee @me`
- **Close**: `gh issue close <number> --repo LioRael/lenso --comment "..."`

Use a heredoc for multi-line bodies. Read the full body, comments, labels,
assignees, parent relationship, blockers, and linked pull requests before
changing tracker state.

When issue-body `Blocked by` prose and GitHub dependency metadata disagree,
treat the issue body as authoritative and verify the relevant blocker merge
commit is reachable from the fetched target base.

## Cross-repository pull requests

Implementation branches and pull requests for this repository target
`LioRael/lenso-organization-plugin`.

A pull request that implements a central Lenso ticket must use repo-qualified
merge-time closure syntax:

```text
Closes LioRael/lenso#<ticket-number>
```

Keep the central ticket open until the implementation pull request merges.
Use the ticket title as the pull-request title unless another repository
contract overrides it.

## Pull requests as a triage surface

**PRs as a request surface: no.**

External pull requests are not placed into the issue-triage queue.

## When a skill says “publish to the issue tracker”

Create an issue in `LioRael/lenso`.

## When a skill says “fetch the relevant ticket”

Run:

```sh
gh issue view <number> --repo LioRael/lenso --comments
```

## Wayfinding operations

The map and its child tickets live in `LioRael/lenso`.

- **Map**: one issue labelled `wayfinder:map`.
- **Child ticket**: a GitHub sub-issue, or a task-list entry with
  `Part of #<map>` when sub-issues are unavailable.
- **Blocking**: GitHub native issue dependencies, falling back to an explicit
  `Blocked by: #<number>` line.
- **Frontier**: the first open, unassigned child without an open blocker.
- **Claim**: assign the ticket to `@me`.
- **Resolve**: comment with the result and close only when the workflow
  explicitly calls for direct closure.
