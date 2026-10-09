# Performance budgets

Ion's main promise is efficient use of the machine. These are the targets each
release is measured against. Automated measurement is planned; until then,
check them by hand with Task Manager or Process Explorer.

| Scenario | Budget |
|---|---|
| Idle, window open, no input | ~0% CPU, no wakeups from Ion timers |
| Idle memory, small project open | < 60 MB |
| Keystroke to frame | < 1 frame (8 ms at 120 Hz) |
| Scrolling a 100k-line file | No dropped frames |
| Indexing a large repository | Uses all cores, then returns to idle |

## Known issues

- **GPUI vsync thread on Windows.** GPUI 0.2.2 runs a `VSyncProvider` thread
  that wakes on every display refresh and messages the UI thread, even when no
  window needs redrawing. Measured on an empty Ion window (dev build): about 4%
  of one core and about 85 MB when idle. Fixing it means making vsync
  on-demand (sleep when no window is dirty, wake on invalidate) in a patched
  GPUI via `[patch.crates-io]`, ideally upstreamed to Zed.

## What counts as a regression

- Any timer or polling loop that runs while the user does nothing.
- Any new long-lived thread or runtime outside the GPUI executor and the file watcher.
- Work on the UI thread that scales with project or file size.
