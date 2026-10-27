---
Status: Current
Created: 2026-06-24
Last edited: 2026-07-18
---

# File list

The file navigator: a directory tree that opens a file in the read pane. It lists the scope's changed files in `Changes`.

## Overview

The list groups files into a collapsible tree. A file row shows a change marker, its name, and its add/remove stats.

```
 src/
   M app.rs                    +562 −16
   A diff_view.rs              +210
   M ui.rs                     +437 −9
 specs/
   A diff-view.md              +96
   M …/2026-06-23-changes/plan +4 −2
 M  Cargo.toml                 +11 −1
 ?  herdr-plugin.toml          +25
```

### Node

The list is a flat sequence of visible rows over the tree.

| field       | type     | meaning                                                             |
| ----------- | -------- | -------------------------------------------------------------------- |
| `kind`      | enum     | `dir` or `file`                                                       |
| `name`      | string   | the segment shown: a directory name, or a file's basename             |
| `depth`     | integer  | nesting level, for indentation                                        |
| `change`    | enum?    | `added`/`modified`/`deleted`/`renamed`/`untracked`, absent on a `dir` |
| `additions` | integer? | lines added in the scope, absent on a `dir`                           |
| `deletions` | integer? | lines removed in the scope, absent on a `dir`                         |
| `expanded`  | bool?    | on a `dir`, whether its children are shown                            |

## Behavior

### Tree

- Files group by directory. Directories sort first, then files, both alphabetical.
- A directory with a single child collapses into its child's row, so vertical space goes to names, not scaffolding.
- Directories open expanded. A changeset is small enough to show whole.

### Selection

- The cursor selects a row. `j`/`k` and the arrows move it, skipping collapsed subtrees. The list scrolls to keep it visible.
- The hunk steps and the file skips move the cursor onto the file they open, from either pane (`input.md`).
- Moving onto a file opens its diff in the read pane.
- The wheel scrolls the viewport only. The selection and the open file stay put, so browsing never reloads a diff.
- `←`/`→` or a click collapses and expands a directory. A click on a file selects and opens it. There is no `enter` activation.
- `tab` moves focus to the read pane, to navigate and comment.
- A poll preserves the selection and expansions by path. A selected file that left the changeset falls back to the open file, then the first file.

### Presentation

- A file row is `<marker> <name> <stats>`: the marker colored by kind, the basename bright, parent directories dimmed, stats right-aligned.
- Stats read `+added −removed`: additions green, deletions red, a zero side dropped. A change with no countable lines (a binary file) shows no stats.
- A too-narrow path shortens with a middle ellipsis (`…/2026-06-23-changes/plan`), keeping the basename and stats visible.

## Non-goals

- No reviewed-file state. Marking a file reviewed and greying it is roadmap.
- No file content rendered here. The read pane renders the diff (`diff-view.md`).
- No whole-worktree browser. Only the scope's changes are listed.

## Related specs

- [review-model](./review-model.md)
- [input](./input.md)
- [tui](./tui.md)
