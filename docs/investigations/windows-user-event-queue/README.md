# Windows user-event queue: September 9 investigation

Both affected Alacritty processes have application events stranded in winit's channel while their main threads wait for Windows messages. The retained dumps establish this failure state. A standalone reproduction establishes a defect in winit 0.30.13 that produces it: `EventLoopProxy::send_event` ignores a failed native wake notification, and the receiver requires one notification for each queued event.

The included patch removes that dependency and passes the failing reproduction, concurrency/lifecycle scenarios, and the combined Alacritty tests. Research began on `fix/windows-event-delivery` at `abdec263`. On September 11, the exact patched crate was vendored and wired into the workspace Cargo manifest for reproducible builds. The verifier now checks the full vendored file inventory and tests that production dependency. The user authorized local executable replacement and explicitly retained terminal focus reporting. Local build/deployment evidence is recorded separately from public publication.

## Evidence from the affected processes

Both PIDs run the staged August 31 executable, integration `737b44bcb64ca6d04187b8b88f7f16fd30a65ae1`, SHA-256 `BAFC778EBC8F9A99AB58DC175BBDBCABAFCEB4F834364CB95D0717CEFE933704`. The earlier application-driven Windows redraw fix is present.

| Process | Pending winit user events | Contents relevant to the symptom |
| --- | ---: | --- |
| 59084 | 26,147 | 26,100 Wakeup; 40 MouseCursorDirty; 4 Frame; 1 ChildExit; **2 Exit** |
| 114648 | 3,226 | 2,950 CursorBlinkingChange; 183 Title; 84 Wakeup; **3 Frame**; 6 MouseCursorDirty |

These are unread, written slots in the actual `std::sync::mpsc` channel, not terminal scrollback or an estimated queue size. The dump reader validates slot publication state and the final block pointer against the channel tail. Offsets were recovered from the matching release disassembly; `terminal-dispatch.log` independently confirms the special handling of terminal tags 9 (Wakeup) and 11 (Exit). Rust enum layouts are build-specific; do not reuse this decoder for another binary.

PID 59084 has no remaining shell/PTY worker, and its main thread is waiting rather than running teardown. The queued Exit events explain why the terminal remains. The close button also takes this path: `WindowEvent::CloseRequested` calls `Term::exit`, which sends another Terminal Exit through the same proxy. Sending another close request cannot repair the missing notifications.

PID 114648 has pending frame, title and terminal notifications while its terminal model and external UIA text contain the final result. This establishes an internal delivery failure capable of producing the reported display lag. The direct `PrintWindow` capture already showed the final result and can itself prompt painting. Therefore the retained screenshot does not isolate every part of the physical display/Magnifier path. No evidence establishes Magnifier or NVDA as the cause of the event backlog.

Local incident evidence is in [`artifacts/2026-09-09-live-hangs`](../../../../artifacts/2026-09-09-live-hangs): `event-backlogs.json`, `read_event_backlog.py`, `channel-layout.log`, `slots.log`, `terminal-dispatch.log`, both process dumps and stacks, UIA text, and captures. Each dump skipped one unreadable 4 KiB page; the channel reads used here succeeded and their consistency checks passed.

## Failure mechanism

1. winit enqueues the application event in an unbounded Rust channel.
2. It calls `PostMessageW` to notify its message-only window and ignores the return value.
3. Each native notification causes exactly one channel receive.
4. If a notification is lost, its payload stays queued. Later notifications consume older payloads and preserve the deficit. After output stops, the GUI thread can sleep with events still pending.

Windows limits a posted-message queue to 10,000 messages by default and reports error 1816 when that quota is exceeded. See [Microsoft's PostMessageW contract](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-postmessagew) and the [winit 0.30.13 Windows implementation](https://docs.rs/winit/0.30.13/src/winit/platform_impl/windows/event_loop.rs.html).

The historical failed API calls were not instrumented in the running release, so their exact timestamp/error cannot be recovered from these dumps. Queue exhaustion is the directly reproduced trigger; the dumps prove the corresponding stranded-work state. This is separate from the previously corrected low-priority WM_PAINT starvation: that fix still needs Terminal Wakeup and Frame events to reach Alacritty.

## Candidate solution

[`winit-0.30.13-pending-events.patch`](winit-0.30.13-pending-events.patch) changes only the Windows backend:

- Track pending user events independently of native messages, reserving the count before publishing to avoid a concurrent receive underflow.
- Post a wake on the transition from no pending work to pending work, instead of one native message per payload.
- Drain finite batches of up to 256 events on wake and normal event-loop boundaries. Empty/stale notifications are harmless, including in `pump_events`.
- Check pending work after `AboutToWait` and before the native wait. Pending work prevents sleeping; an empty queue keeps normal blocking behavior.
- Keep `Resumed` ahead of pre-start user events and honor exit requested while draining or in `AboutToWait`.

The invariant is that application work controls whether the outer loop may sleep. A failed wake on a full native queue is recoverable: queued native messages already wake the thread, and pending work is checked before it can sleep again. A send racing after the pending check either posts a wake or encounters an already nonempty native queue. No periodic idle polling or queue-limit registry change is required.

The payload queue remains lossless and FIFO; this patch does not discard terminal events or change accessibility. Reserving before publishing can cause brief active polling if a sender is preempted between those operations. The batch bound prevents one drain from consuming an unlimited producer stream; it is not a global scheduling guarantee. Other winit native-message users, such as its thread executor, are outside this patch's scope.

## Reproduction and verification

The standalone probe has no terminal, renderer, NVDA dependency, or user input. It sends numbered events through the public winit API and checks delivery, per-producer FIFO, startup ordering and exit.

| Scenario | Unpatched winit | Candidate |
| --- | --- | --- |
| 20,000-event burst | All sends report success; only 10,000 delivered | All 20,000 delivered |
| Native queue prefilled with 10,000 WM_NULL messages; then 20,000 events | Error 1816 confirmed; all sends report success; zero delivered | All 20,000 delivered |
| Four concurrent producers after an idle period | Not used as RED gate | All 80,000 delivered in producer order |
| 20,000 events before startup | Not used as RED gate | All delivered after Resumed |
| Exit after final event, normal/full native queue | Not used as RED gate | Natural return without deadline rescue |
| Pump API with full queue/concurrent producers/exit | Not used as RED gate | Complete delivery and return |

Nine candidate scenarios passed. Empty-queue spinning is guarded by a wait-cycle limit in the non-pump burst cases; this is not a comprehensive CPU benchmark. The first startup candidate failed the Resumed-order assertion and was corrected. An early shutdown check measured total workload time rather than exit latency; it was replaced with a timestamp at the actual exit request. The earlier failed logs are retained, not counted as passes.

Run the retained proof on Windows with Python, Git, Rust and the cached Cargo dependencies:

```powershell
python .\docs\investigations\windows-user-event-queue\verify.py
```

The script copies the crate, validates hashes in `candidate.json`, checks/applies the patch, builds separate baseline/candidate binaries and retains every result. It never edits the Cargo registry. `--winit-source` can name an unpacked 0.30.13 crate and `--output` selects the proof directory. Baseline exit 101 is expected specifically for the missing-delivery assertions; every candidate must exit zero. The repro is an independent Cargo workspace.

On the combined disposable checkout at `737b44bc`, with the patched dependency supplied through Cargo's command-line patch override:

- All **169 Alacritty tests passed**, including accessibility and prior redraw regressions.
- The Windows debug executable built successfully.
- A separate owned Alacritty process consumed 20,000 OSC title changes and 40,000 cursor-blink changes. Its final native title and external UIA text agreed after 2.179 seconds. The captured frame also showed `T020-T021-FINAL-20000` in black on white. It exited naturally with code zero after the child finished its intentional 15-second hold (15.709 seconds total).

See `alacritty-final-candidate-tests.log`, `alacritty-candidate-build.log`, `full-app-result.json`, `full-app-uia.txt`, and `full-app-final.bmp`. Initial full-app probes had UIA discovery and fixed-title setup mistakes; the final probe uses the owned HWND and allows dynamic titles. Direct capture can prompt repaint, so it is supplementary evidence rather than proof of uninterrupted physical display freshness. NVDA was running; actual speech and Magnifier output were not asserted.

## Release path and remaining acceptance

Focus follow-up: the user reports that background taskbar titles remain in a working state and activation visibly advances output/title. Static tracing of the exact staged source found no focus gate in the PTY reader, winit's native-message dispatch/wait, Alacritty's all-window `AboutToWait` processing, or the terminal-title handler. Focus loss does cancel cursor-blink timers (`event.rs`, `update_cursor_blinking`); with no other deadlines, the application requests an indefinite `ControlFlow::Wait`. Focus gain can mark the display dirty, restart blinking when configured, and send `ESC [ I` to a child that enabled focus reporting (`input/mod.rs`, `on_focus_change`). These are possible recovery stimuli, not evidence of deliberate background suspension. Alacritty also stages ordinary events in a per-window batch, flushed at AboutToWait or RedrawRequested; this is distinct from the stranded winit channel decoded above. An unrelated native focus/paint message does not itself consume every pending winit payload. The exact activation-to-recovery sequence remains to be traced; do not claim the observed complete catch-up is proven solely by the queue-deficit model. The draw occlusion guard does not check focus, and winit 0.30.13 documents Occluded as unsupported on Windows.

Carry this exact patch through a pinned winit dependency (a reviewed fork revision or vendored crate), with its regression probe, on the owning Windows event-delivery source branch. Reconstruct the merge-only integration candidate from pinned source tips and run the normal Windows gate and release build. Do not rely on an edited local Cargo cache or an undocumented command-line override for distribution.

Codex focus follow-up: the supplied clean checkout `F:/projects/3rdParty/AI-Related/Harnesses/codex` at `c77c34ed33877a6e5b3759703d01d3b223274cbf` enables terminal focus reporting under `cfg(not(windows))` and explicitly disables it under `cfg(windows)` (`codex-rs/tui/src/tui.rs:244`). Its event stream maps FocusGained/FocusLost into focus state; FocusGained enters the normal draw branch (`app.rs:948`), including pre-draw work and title-progress synchronization. Ordinary draw scheduling has no terminal-focus gate in the inspected paths. The focus flag additionally governs notifications and recap state. Windows process inspection confirms PID 114648 -> pwsh 3468 -> bash 195124 -> wsl 130932, making the Linux/WSL behavior relevant, but the running Codex binary has not been matched to this checkout revision. This establishes a possible source of fresh output on activation, not proof of the exact historic recovery sequence.

A background-only periodic timer is not required by the candidate: its pending-count check and bounded drains apply regardless of focus. If a watchdog is added later, it must invoke actual draining on the event-loop thread and wake independently of the broken payload notification path. Posting a new ordinary proxy event per tick can advance older entries while preserving the deficit; that alone is not queue recovery. An idle polling timer also introduces periodic wakeups and recovery latency. Prefer the existing pending-work invariant; use an independent timer only for a separately demonstrated need or diagnostic observation.

Before calling T-020/T-021 fully accepted, verify ordinary close after a burst, sustained output and idle transitions, resize/modal-loop behavior, and actual NVDA speech plus physical pixels with the user's normal Magnifier configuration. The full-app probe proves natural child exit and external UIA; it does not replace those checks. No user process was stopped, no staged executable was replaced, and no public branch was changed. The supplied binary is a debug research build, not a promoted release.
