# Tasks
Next task ID: T-008

## Summary
Open tasks: 1 (In Progress: 0, Next Today: 0, Next This Week: 0, Next Later: 1, Blocked: 0)
Done tasks: 6

## In Progress

## Next – Today

## Next – This Week

## Next – Later

### T-007 [A11Y] Add selection and optional scrollback support
Outcome:
- Alacritty text selection can be exposed through UIA `GetSelection`
- optional scrollback range reads remain performant and do not regress visible viewport navigation
- color and style attributes remain out of scope
Proof:
- Run: `cargo test -p alacritty accessibility::selection`
  Expect: all pass
- Run: `cargo check -p alacritty --target x86_64-pc-windows-msvc`
  Expect: exit=0
- Run: `powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\accessibility\probe-uia-selection.ps1`
  Expect: exit=0, stdout contains "Selection: PASS"
Touches: alacritty/src/accessibility/, alacritty/src/display/, scripts/accessibility/
Deps: T-004, T-005, T-006
Verify: unit-test, cli-proof, manual
Notes: Source plan Slice 7. This is a follow-up after visible viewport reading and mouse hit testing are stable.

## Blocked

## Done

### T-006 [A11Y] Throttle UIA text and caret events
Outcome:
- UIA events are skipped when `UiaClientsAreListening()` is false
- terminal text changes are coalesced to at most one event per throttle interval per window
- caret/cursor updates coalesce to the latest known position
- sustained output does not emit one UIA event per byte, cell, line, parser update, or frame
Proof:
- Run: `cargo test -p alacritty accessibility::event_throttle`
  Expect: all pass
- Run: `cargo check -p alacritty --target x86_64-pc-windows-msvc`
  Expect: exit=0
- Run: `powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\accessibility\probe-uia-event-throttle.ps1`
  Expect: exit=0, stdout contains "Event coalescing: PASS"
Touches: alacritty/src/accessibility/, alacritty/src/window_context.rs, scripts/accessibility/
Deps: T-003, T-004
Verify: unit-test, cli-proof, manual
Notes: Source plan Slice 6. Start with a 50-100 ms internal throttle and tune from evidence; do not rely on NVDA alone for flood control.

### T-005 [A11Y] Map mouse coordinates with UIA RangeFromPoint
Outcome:
- `RangeFromPoint` maps screen coordinates inside the text area to the nearest terminal row and column
- points in padding or outside exact cell bounds clamp to a deterministic nearest text range
- a client can expand the returned range to a line and read the row under the mouse
Proof:
- Run: `cargo test -p alacritty accessibility::range_from_point`
  Expect: all pass
- Run: `cargo check -p alacritty --target x86_64-pc-windows-msvc`
  Expect: exit=0
- Run: `powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\accessibility\probe-uia-range-from-point.ps1`
  Expect: exit=0, stdout contains "RangeFromPoint line read: PASS"
Touches: alacritty/src/accessibility/, alacritty/src/display/, scripts/accessibility/
Deps: T-003
Verify: unit-test, cli-proof, manual
Notes: Source plan Slice 5. Keep this as standard UIA hit testing; do not add custom speech to Alacritty.

### T-004 [A11Y] Implement UIA text range navigation
Outcome:
- UIA text ranges support clone, endpoint comparison, endpoint movement, enclosing-unit expansion, and text retrieval
- line, word, and character navigation operate predictably across empty rows and wrapped rows
- Terminal Access for NVDA current, previous, and next line commands can read Alacritty output
Proof:
- Run: `cargo test -p alacritty accessibility::text_range`
  Expect: all pass
- Run: `cargo check -p alacritty --target x86_64-pc-windows-msvc`
  Expect: exit=0
- Run: `powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\accessibility\probe-uia-navigation.ps1`
  Expect: exit=0, stdout contains "Line navigation: PASS"
Touches: alacritty/src/accessibility/, scripts/accessibility/
Deps: T-003
Verify: unit-test, cli-proof, manual
Notes: Source plan Slice 4. Keep implementation focused on units Terminal Access exercises first.

### T-003 [A11Y] Expose visible text through UIA TextPattern
Outcome:
- UIA `GetCurrentPattern(UIA_TextPatternId)` succeeds for the Alacritty window
- `DocumentRange.GetText(-1)` returns visible terminal text
- `GetVisibleRanges()` returns the visible viewport range
- `GetSelection()` returns empty or no selection until selection support is implemented
Proof:
- Run: `cargo test -p alacritty accessibility::text_pattern`
  Expect: all pass
- Run: `cargo check -p alacritty --target x86_64-pc-windows-msvc`
  Expect: exit=0
- Run: `powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\accessibility\probe-uia-textpattern.ps1`
  Expect: exit=0, stdout contains "DocumentRange.GetText: PASS"
Touches: alacritty/src/accessibility/, scripts/accessibility/
Deps: T-002
Verify: unit-test, cli-proof, build-only
Notes: Source plan Slice 3. The probe may be added as part of this task if no equivalent exists.

### T-002 [A11Y] Attach Windows UIA provider to Alacritty HWND
Outcome:
- Windows builds create a per-window UI Automation provider for each Alacritty HWND
- `WM_GETOBJECT` returns the provider through `UiaReturnRawElementProvider`
- provider exposes sensible terminal element properties and is disconnected on window destruction
Proof:
- Run: `cargo check -p alacritty --target x86_64-pc-windows-msvc`
  Expect: exit=0
- Run: `cargo test -p alacritty accessibility::windows_provider`
  Expect: all pass
Touches: alacritty/Cargo.toml, alacritty/src/accessibility/, alacritty/src/display/window.rs
Deps: T-001
Verify: unit-test, build-only
Notes: Source plan Slice 2. Prefer winit Windows hooks if available; otherwise subclass the HWND and restore the previous window procedure on drop.

### T-001 [A11Y] Add visible terminal snapshot model
Outcome:
- A snapshot model produces newline-separated visible terminal text from `Term`
- snapshot mapping supports row/column to text-offset and text-offset to row/column conversion
- snapshot handling covers wide cells, spacer cells, hidden cells, empty rows, cursor position, and row extraction
Proof:
- Run: `cargo test -p alacritty accessibility::snapshot`
  Expect: all pass
- Run: `cargo check -p alacritty --target x86_64-pc-windows-msvc`
  Expect: exit=0
Touches: alacritty/src/accessibility/, alacritty/src/window_context.rs
Verify: unit-test, build-only
Notes: Source plan: `.agents/plans/2026-06-06-nvda-accessibility-plan.md` Slice 1. Do not include color/style attributes.
