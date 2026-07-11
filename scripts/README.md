Scripts
=======

## Flamegraph

Run the release version of Alacritty while recording call stacks. After the
Alacritty process exits, a flamegraph will be generated and it's URI printed
as the only output to STDOUT.

```sh
./create-flamegraph.sh
```

Running this script depends on an installation of `perf`.

## ANSI Color Tests

We include a few scripts for testing the color of text inside a terminal. The
first shows various foreground and background variants. The second enumerates
all the colors of a standard terminal. The third enumerates the 24-bit colors.

```sh
./fg-bg.sh
./colors.sh
./24-bit-colors.sh
```

## Windows Accessibility Gate

Run the local Windows validation gate for accessibility work. This checks Rust
formatting with nightly rustfmt, runs Clippy for the Windows MSVC target, checks
the Alacritty binary crate for that target, and runs the terminal crate tests.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-windows-accessibility.ps1
```
