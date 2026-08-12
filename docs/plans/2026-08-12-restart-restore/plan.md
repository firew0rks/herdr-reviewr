# Bring reviewr panes back after a herdr restart

## Problem

A herdr server restart leaves every reviewr pane as a bare shell. The pane, its label, its
place in the layout and its cwd all come back; only the review UI is gone.

Measured on herdr 0.7.5 in a throwaway session (`herdr --session rtest`):

- The snapshot keeps the pane, its `reviewr` label and — since 0.7.5 — the plugin pane's
  `launch_argv` (`sh -c 'exec "$HERDR_PLUGIN_ROOT/bin/herdr-reviewr"'`).
- On restore herdr spawns the **user's shell** for that pane anyway: one `pane.spawn.start`
  per pane, and the restored child is `/bin/zsh`, not `sh`. `launch_argv` is not replayed for
  a plugin pane, and `$HERDR_PLUGIN_ROOT` would be unset in the restored env regardless.
- herdr's plugin-pane registry is already documented as not surviving a restart
  (`specs/herdr-host.md`), which is why `close` sweeps by live process rather than the registry.

So nothing in herdr will relaunch the UI. The plugin has to.

## Approach

A one-shot `[[startup]]` hook (herdr 0.7.5) that re-execs the review UI in the panes herdr
just restored as shells.

- `herdr/pane.sh restore` — list every pane in the session, keep those labelled `reviewr`
  that are **not** running the UI in their foreground process group (the existing
  `is_reviewr_pane` read), and `herdr pane run <pane> exec <binary>` each one. The shell is
  replaced in place, so the pane keeps its id, layout share, cwd and scrollback.
- The label is the only surviving marker of "this was a reviewr pane" — the process is gone
  and the registry with it. Everywhere else the label stays display-only, per upstream.
- Idempotent by construction: a pane already running the UI is skipped, so a re-fired hook
  (live handoff) is a no-op.
- Startup hooks fire after session restore (measured: restore at `.438`, hook at `.583`), but
  a restored shell may still be reading its rc when the text arrives. The sweep verifies each
  pane with a second process read and re-sends, bounded.

## Steps

1. [x] Rebase `feat/issues-tab` onto `upstream/main` (v0.30.1). The fourth tab's ten columns
   evicted the scope chip from a 40-column header, so the strip now yields to it exactly as it
   yields to the PR chip, from one cap the paint and the hit test share.
2. [x] `herdr/pane.sh`: `restore` mode + session-wide pane list.
3. [x] `herdr-plugin.toml`: `[[startup]]`, plus a `restore` action for a sweep by hand.
4. [x] `specs/herdr-host.md`: restore is part of the pane lifecycle contract (`HH-RESTART-WHOLE`).
5. [x] `tests/pane_actions.rs`: relaunches only non-UI `reviewr` panes, no-op on a re-fire,
   leaves a busy pane alone, names one that never came back.
6. [x] `just ci`, `just qa-install`, proved on four restarts of a throwaway session.

## What the build taught

- **The idle read cannot compare process groups.** The first guard called a pane busy when its
  foreground group differed from `shell_pid`. A shell sourcing its rc holds a group of its own
  for a few hundred milliseconds, which is exactly when the startup hook fires: measured, one
  restart in two skipped the pane with `left running`. The read is by process name now.
- **One send, then confirm.** A retry would land as keystrokes in a UI that started late.

## Done when

- A herdr restart brings every reviewr pane back on its own.
- The fork is on top of v0.30.1 with the issues tab intact and `just ci` green.
