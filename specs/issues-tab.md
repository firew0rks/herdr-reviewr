---
Status: Current
Created: 2026-07-29
Last edited: 2026-08-12
---

# Issues tab

A read-only list of the issues the resolved pull request closes and their conversations, in the
sidebar's two-pane frame: the rows in the navigator, the selected body in the read pane.

## Overview

The navigator lists every issue the PR closes, each followed by its own comments. The read pane
shows the selected issue's body, or the selected comment. The tab reads the forge through
`forge-host.md` and writes nothing; its only outward action opens a link
in the browser. The header is the PR tab's, unchanged — the tab describes the same pull request.

The tab admits closing references only. An issue named in the PR description without a closing
keyword is context the reader can already see in the description; an issue the PR closes is the
work being finished, and only that set answers "was this what we asked for".

```
 1 Changes  2 Files  3 PR  4 Issues    Care-team knowledge-base gap…  fix/6440  open #7080 ↗
╭─ #6440 Agent confabulates an Invite button ─────────╮╭─ Linked issues ──────────────╮
│ A village-worker asked how to add someone to the    ││▍#6440 Agent confabulates… open│
│ care team. The agent described an Invite button     ││   @JordanIndi            2d  │
│ that does not exist in that role's UI.              ││   @sing                  5d  │
│                                                     ││ #7113 Records UI-lock…  closed│
│ ## Acceptance                                       ││                              │
│ - village-worker sees only its own notes            ││                              │
╰─────────────────────────────────────────────────────╯╰──────────────────────────────╯
 ⚠ conflicts with main · ✓ passing · 2 issues            o open ↗                            ?
```

## Invariants

| code            | Always true                                                                  |
| --------------- | ---------------------------------------------------------------------------- |
| `IT-NO-FETCH`   | The issues and their comments arrive on the PR snapshot; the tab starts no fetch. |
| `IT-FLAT`       | Every row is reachable by one cursor; nothing expands or collapses.           |
| `IT-CLOSING`    | Only closing references are listed, never issues the description mentions.    |
| `IT-READ-ONLY`  | The tab writes nothing to the forge; `o` opens a link and nothing else.       |

`IT-NO-FETCH` forbids both an issue query keyed on the branch and a per-issue detail call: either
can disagree with the PR tab about which PR is current, and either adds a second refresh clock to
reconcile with `forge-host.md`'s.

## Behavior

### Navigator and read pane

- The navigator, titled `Linked issues`, lists one row per issue — `#number title state` — each
  followed by its own comments, indented, `@author age`.
- Every issue's comments are always listed. `IT-FLAT` forbids a drill-down and a cursor-following
  expansion: the first hides the conversation the tab exists to show, and the second makes the row
  list depend on the selection that indexes it.
- Comments sort newest first, like the PR tab's.
- Issues sort by number descending. The forge returns them in link order, which records the order
  someone typed `Closes #n` and is not information.
- A closed issue dims its state word rather than leaving the list. A PR that closes an issue
  already closed is worth seeing.
- The read pane shows the selected issue's body, or the selected comment's, as markdown
  (`markdown.md`). A human author is emphasized over the bots, as on the PR tab.
- `j`/`k` or a click selects a row and resets the read pane to the top.
- `o` on a comment opens its issue: a comment is read here, not linked to separately.
- The wheel over either pane scrolls that pane. `PageUp`/`PageDown` scroll the focused pane.
- The tab keeps its own cursor and both scrolls, so returning to it lands where you left it
  (`tui.md`).
- The authoring keys (`s`, `c`, `v`, `d`, `e`) do nothing here, as on the PR tab.

### Empty states

| condition                              | read pane                                                |
| -------------------------------------- | -------------------------------------------------------- |
| a PR resolved, closing no issues       | `No issues linked to this pull request. Link one with "Closes #n".` |
| no PR, detached HEAD, loading, degraded | the PR tab's message for that state (`pr-tab.md`)        |

A state other than "resolved, none linked" belongs to the pull request, not to its issues, so the
two tabs say the same thing about it and cannot drift.

### Forges

GitHub fills the list. GitLab and Azure DevOps return none and show the resolved-PR empty state,
which is the tab's ordinary "nothing linked" path rather than an error — an unimplemented read and
an empty result are indistinguishable to the reader, and claiming a failure would be a lie.

## Related specs

- [forge-host](./forge-host.md) — the snapshot the issues ride on
- [forge-providers](./forge-providers.md) — the per-forge query
- [pr-tab](./pr-tab.md) — the header, and the empty states this tab defers to
- [tui](./tui.md) — the tab strip and per-tab place state
- [input](./input.md) — the `tab-issues` binding
