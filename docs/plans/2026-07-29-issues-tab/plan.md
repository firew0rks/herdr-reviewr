# Linked issues tab: Plan

Delivers a fourth tab showing the issues the current PR closes, read-only, in the existing
two-pane frame.

## Problem

The PR tab mirrors the pull request, but the work the PR exists to finish lives in an issue the
reader has to leave the pane to read. Reviewing an agent's diff against the PR description alone
answers "what changed", never "was this what we asked for". The acceptance criteria, the
reproduction, and the scope live in the issue.

`closingIssuesReferences` already distinguishes what a PR *closes* from what its body merely
mentions, and the two differ in practice: on `the-indi-app/project-indi`, PR #7080 links #6440
while its body also names #7113; PR #7555 links nothing while naming four issues as context.
Scraping the body would show the wrong set.

## Goal

A `4 Issues` tab lists every issue the resolved PR closes and shows the selected issue's body,
read-only, using the same navigator/read-pane frame and the same fetch worker as the PR tab.

## Definition of Done

- [ ] `4` selects the tab; the tab strip shows it on every forge.
- [ ] The navigator lists every linked issue, newest-first by number, each row `#number title state`.
- [ ] The read pane renders the selected issue's body as markdown (`markdown.md`).
- [ ] `j`/`k` and a click move the selection; the wheel scrolls each pane independently.
- [ ] `o` opens the selected issue in the browser.
- [ ] A PR that links no issues shows an empty state naming the forge's noun.
- [ ] No PR at all shows the PR tab's existing empty state, worded for issues.
- [ ] The issues ride the PR snapshot: one resolution, no second poll loop, no new timer.
- [ ] GitLab and Azure DevOps return an empty list and render the empty state; neither errors.
- [ ] The authoring keys (`s`, `c`, `v`, `d`, `e`) do nothing here, as on the PR tab.
- [ ] `just ci` green: fmt, clippy with warnings as errors, tests, release build.

## Out of Scope

- Writing to issues. No commenting, closing, or labelling — `specs/tui.md` non-goals stand.
- Issues that are merely mentioned in the body. Only `closingIssuesReferences`.
- Issues with no PR. The tab is the PR's context, not an issue browser.
- GitLab and Azure DevOps linked-issue queries. Routed and degrading, not implemented.
- Issue comments. The body only; the PR tab already owns the conversation.

## Design decisions

**One list, selection switches** — not a toggle between whole views. The frame is already
navigator-lists / read-pane-shows-one, and the PR tab's comments prove the shape. A toggle would
invent a second interaction for a problem the frame already solves.

**A tab, not a section in the PR navigator.** A section is cheaper and issues are PR context, so
it is the real alternative. It loses because issue bodies are long-form acceptance criteria and
the PR navigator is already checks + description + comments; a third section pushes the reader
into a scrolling list to reach the thing they opened it for.

**Issues hang off the PR snapshot.** `PrSnapshot` (`src/forge.rs:121`) gains
`issues: Vec<LinkedIssue>` beside `checks` and `comments`, filled by the same fetch. The
alternative — an independent issue fetch keyed on the branch — needs its own resolution, its own
refresh gate, and can disagree with the PR tab about which PR is current.

**One GraphQL read, not N.** `closingIssuesReferences` returns `id`, `number`, `repository`, `url`
and no title, state, or body. Per-issue `gh issue view` calls cost one round trip each on a PR
that closes several. A single `gh api graphql` fetching the PR and its linked issues' fields
together keeps the tab on one call, matching the "one fetch per poll" shape the PR tab holds.

## Execution Plan

1. `specs/issues-tab.md` (`IT`) — new spec, modelled on `pr-tab.md`: header, navigator, read pane,
   empty states. Add it to the `specs/README.md` map.
2. `specs/forge-host.md` — the snapshot gains a `issues` row; state the admission rule
   (closing references only) and the cap.
3. `specs/forge-providers.md` — GitHub's query; GitLab and Azure declare an empty list.
4. `specs/tui.md` and `specs/input.md` — the fourth tab and the `4` binding.
5. `src/forge.rs` — `LinkedIssue`, `PrSnapshot.issues`, and the GitHub read in `fetch_inner`'s
   per-forge dispatch. GitLab/Azure return empty.
6. `src/keymap.rs` — `Action::TabIssues`, bound to `4`.
7. `src/app.rs` — `Tab::Issues`, selection state, and the `app.tab == Tab::Pr` gates that must
   also admit Issues (poll scheduling, refresh glyph, wait timer — ~10 sites in `lib.rs`).
8. `src/ui.rs` — the tab strip entry and the navigator/read-pane render.
9. `CHANGELOG.md` — one bullet under `## [Unreleased]`.
10. `just ci`, then `just qa-install` to test in real herdr panes.

## Risks

- **The `Tab::Pr` gates are load-bearing.** `lib.rs` tests polling, the refresh glyph, and the wait
  timer against `Tab::Pr` in about ten places. Each is a decision about whether the *forge* is
  being shown, not whether the PR tab is. Missing one gives a tab that never refreshes.
- **Upstream will want forge parity.** `fetch_inner` documents exhaustive per-forge dispatch, so
  the code forces the routing, but a GitHub-only feature may not be acceptable upstream even when
  the other two degrade cleanly. Worth agreeing before the PR.
