# Windows taskbar reentry deadlock

On October 8, 2026, five running diagnostic-build terminals reported as
unresponsive. Noninvasive CDB captures of PIDs 11820 and 30928 show the main
thread waiting for winit's WindowState mutex inside a nested window callback.
The outer callback holds the same mutex across `set_skip_taskbar`, whose
`ITaskbarList::AddTab` call enters Explorer and synchronously dispatches another
window message. The mutex is not reentrant, so the GUI thread cannot return.
The title-log heartbeat continues while GUI tick counts stop advancing.

Raw captures are retained in the workspace's
`artifacts/2026-10-08-live-hangs/stacks-{11820,30928}.log`. Existing terminals
were neither terminated nor modified. Their historical triggering message and
Explorer lifecycle were not recorded; the nested lock and shell-call path are
directly observed.

The fix copies `skip_taskbar` out of the mutex guard in its own statement before
calling the shell. Taskbar visibility behavior and accessibility remain enabled.
No dependency upgrade or production test hook is needed.

## Deterministic regression

`taskbar_created_allows_synchronous_window_reentry` creates a hidden native
winit window and dispatches TaskbarCreated through the real window procedure.
A test-only ITaskbarList fixture models synchronous shell reentry with
SendMessageW(WM_GETMINMAXINFO), covering both AddTab and DeleteTab. It checks the
mutex first to report an assertion failure instead of hanging the test process.
The real shell's timing cannot be reproduced deterministically: twenty taskbar
notifications on a fresh unpatched terminal did not trigger the hang.

```text
cargo test --manifest-path vendor/winit/Cargo.toml --lib --no-default-features taskbar_created_allows_synchronous_window_reentry --target-dir target/winit-taskbar
```

Before the fix, the regression fails with
`TaskbarCreated held WindowState across shell reentry`. After the fix, it passes
and both nested native callbacks complete. The offline queue verifier also runs
this regression against the production vendored dependency.

## Upstream

Alacritty upstream master at `29dc5537` adds two commits since our `ede2ac14`
baseline: `d692748d` removes an unnecessary terminal row allocation limit, and
`29dc5537` adds the macOS local-network permission description. Neither changes
Windows event delivery. Integrate them through a separate merge-only candidate.

A matching defect was fixed in the related tao backend:
https://github.com/tauri-apps/tao/issues/1263 and
https://github.com/tauri-apps/tao/pull/1264. Our diagnosis is supported by the
local stacks and regression, independently of that report.

## Verification boundary

The regression proves the lock lifetime and native reentry path. It does not
prove recovery of an already deadlocked process, an actual Explorer restart,
sleep/resume, or NVDA speech acceptance. A new executable is required for new
terminal instances. Do not replace an executable used by running terminals.
