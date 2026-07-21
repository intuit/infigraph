---
name: review-pr-against-issue
description: Review one or more PRs against the GitHub issue(s) they claim to fix, including fetching PR branches directly when `gh` can't reach github.com (e.g. gh is authenticated to an enterprise host instead). Use whenever asked "does this PR fix issue #N", "review PR #N against #M", or "will merging these PRs close this issue".
---

# Review a PR against the issue it claims to fix

## 1. Get the issue text

Try `gh issue view <N> --repo intuit/infigraph --json title,body,labels,state`. If `gh auth status` shows you're authenticated to a different host (e.g. `github.intuit.com` instead of `github.com`), `gh` calls will 404/401 even though the repo is public. Either:
- `GH_HOST=github.com gh issue view <N> --repo intuit/infigraph ...` (works if a github.com token exists), or
- `WebFetch` the issue URL directly (`https://github.com/intuit/infigraph/issues/<N>`) — works for public repos with no auth needed.

Extract the issue's **root cause** and, if present, its **numbered list of distinct sub-defects/suggestions** — don't treat the issue as one monolithic bug if it enumerates several. A PR can close the headline symptom while leaving half the listed defects untouched.

## 2. Get the PR

`gh pr view` will hit the same host problem. WebFetch the PR URL for description/stated-issue-link, then pull the actual diff locally (WebFetch can't reliably render diffs):

```bash
git fetch https://github.com/intuit/infigraph.git refs/pull/<N>/head:pr-<N>
git diff main pr-<N> -- . ':!*.lock'
```

This works read-only with no auth, for any public repo, regardless of what host `gh` is configured for. Repeat per PR into separate `pr-<N>` local branches — cheap, and lets you diff them against each other too.

## 3. Match diff to root cause, not just to the title

For each sub-defect in the issue:
- Grep the current (pre-fix, `main`) code for the exact broken behavior described (e.g. a hardcoded flag, a missing guard, a skipped check) to confirm the issue's own description is still accurate before crediting a PR with fixing it.
- Check whether the PR's diff actually touches the file/line the issue points to.
- If the issue names specific "suggested fixes" (plural), check each independently — PRs frequently fix the primary suggestion and skip the rest (logging/diagnostics ones especially tend to get dropped).

Don't assume a PR closes an issue just because its description says "Fixes #N" — verify against the diff.

## 4. When multiple PRs are being merged together

- Check same-author + same-base-commit doesn't imply no-conflict — verify explicitly:
  ```bash
  git checkout -b merge-test-all main
  git merge --no-edit pr-<N>   # repeat per PR
  ```
  Clean merges only guarantee no *textual* conflict, not that combined behavior is correct — still run build+test on the merged tree.
- `cargo build --workspace` then `cargo test --workspace --no-fail-fast` on the merged branch (this repo's CI equivalent — see root CLAUDE.md). Run test in background (`run_in_background: true`) if it's a large workspace; check `.output` file or wait for the completion notification rather than polling.
- Delete the scratch branch/refs when done, or leave them if the user wants to keep testing — don't push them anywhere.

## 5. Report format

Per PR: root-cause match (yes/no/partial), which specific sub-defects it closes vs. leaves open, any gaps in the fix itself. Then an overall verdict — will merging all of them actually close the issue(s), or does it need to stay open / get rescoped to a checklist matching only what's fixed — with a confidence and recommendation per the repo's working convention.
