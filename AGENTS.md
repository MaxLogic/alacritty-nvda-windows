# Hidden files

Do not assume all guidance files are Git-tracked. Some may be untracked or ignored. When searching for instructions, specs, or task files, search beyond Git-visible files: use `rg --hidden --no-ignore --glob '!.git'` when possible, or fall back to `grep`/`find`.

Especially check:
- `AGENTS.md`
- `agents.md`
- `conventions.md`
- `spec*.md`
- `.agents/`
- `TASKS.md`

---

# ast-grep

Prefer ast-grep for structural code search when regex search is too brittle.
Use `sg` for built-in languages.
Use `sgp` for Delphi/Pascal or mixed searches needing Pascal support.
For Pascal patterns, use `_A`, `_B`, `___ARGS` metavariables.

---

# Accessibility

This fork's explicit goal is full NVDA and screen reader compatibility. Accessibility support must be treated as core functionality, not an optional feature.

Do not solve NVDA crashes, UIA failures, or screen reader regressions by disabling the accessibility provider, hiding it behind an opt-in flag, or removing exposed accessibility features. A kill switch or environment gate may be used only as a short-lived diagnostic aid while isolating a crash, and must not be presented as the project solution.

When accessibility behavior is broken, investigate the provider implementation against the relevant accessibility standard and screen reader behavior. Prefer evidence from NVDA logs, Windows UI Automation clients, focused regression tests, and provider-level instrumentation over assumptions. The expected outcome is a correct, stable accessibility implementation that works with NVDA in normal use.
