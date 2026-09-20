# Windows title delivery diagnostics

The September 20 report concerns a Codex working prefix that remained after work
finished and cleared when one window received focus. The running executable is
the September 11 queue-fix build (`a3eb678f`), SHA-256
`3168A7958BE2839071D9D57F12FC0D7D80996A9DDE1C779601722BCBF5D00496`.
The historical final title bytes were not logged. The captured PID 85488 title
was changing while unfocused, so the original stale transition has not been
reproduced or assigned to a confirmed cause.

This candidate adds instrumentation, not a speculative behavior fix. Normal
title handling, focus reporting, accessibility and colors remain enabled.

## Collecting the next incident

Logging starts automatically in Windows candidate processes. Files are under
`%LOCALAPPDATA%\Alacritty\diagnostics`, falling back to
`%TEMP%\Alacritty\diagnostics` if necessary. Each process uses
`title-<PID>-<startup epoch milliseconds>.log` and `.previous.log`, each at most
4 MiB. Old process files are retained and can be removed after investigation.

When the title becomes stale, record the PID and approximate time before
focusing the window. Preserve both files for that PID after any recovery. No
special command-line flags or configuration changes are needed. These logs
contain window titles, process identity, timestamps and counters; they contain
no terminal output or typed input. The writer uses a bounded queue and records
dropped records. A dropped count invalidates assumptions that missing records
necessarily indicate a missing application event. A filesystem or writer-thread
failure stops diagnostic output without interrupting the terminal.

## Reading the records

- `title_send`: terminal parsing emitted a title event for this window.
- `title_send_result`: winit accepted it, or its event loop had closed. This is
  not an acknowledgement that Windows applied the caption.
- `title_received`: the main event loop received the title.
- `title_apply` / `title_reset_apply`: the per-window batch reached its title
  handler. The focus and dynamic/preserved-title fields explain intentional
  suppression.
- `title_window_begin`: title application started, before any accessibility
  name notification in the combined NVDA build.
- `title_native_return`: the native setter returned.
- `title_native_end`: the native caption was
  read back. Readback is capped at 1023 UTF-16 code units; an unusually long
  title can therefore compare unequal because of truncation.
  The own-process `GetWindowTextW` readback can synchronously invoke WM_GETTEXT
  and perturb timing. Matching readback establishes the native caption, not
  physical taskbar presentation.
- `focus_received` / `focus_apply`: native focus delivery and whether reporting
  to the child was enabled. These do not record ordinary keyboard input.
- `heartbeat`: independent five-second writer heartbeat, cumulative PTY bytes,
  GUI event-loop ticks and dropped diagnostic records. A stationary GUI tick
  count can mean normal idle waiting; it is not alone proof of a hang.

Records carry monotonic microseconds; init and heartbeat identify the starting
Unix epoch. The asynchronous writer preserves channel arrival order, but
concurrent send/receive records can interleave. Match window IDs, titles and
timestamps; do not assume `title_send_result` precedes `title_received`.
Records are bounded and single-line; very long title details are truncated.

A received final title without application implicates batching or a blocked
GUI callback. A begin without a native return implicates the accessibility or
native setter path; a return without an end implicates readback. Either warrants
a non-suspending stack capture. Matching native readback with
a stale taskbar presentation directs investigation beyond title delivery. No
final title at `title_send`, with continuous logging and zero drops, directs
investigation toward the producer/ConPTY/parser boundary; it does not by itself
prove a Codex defect.

## Verification scope

The diagnostic writer has focused bounds, rotation and heartbeat tests. The
combined candidate must also pass Alacritty/terminal tests and an unfocused
OSC-title smoke probe against its exact release executable. These checks prove
the instrumentation and normal background delivery; they do not reproduce the
reported intermittent failure or establish physical taskbar/NVDA speech output.
