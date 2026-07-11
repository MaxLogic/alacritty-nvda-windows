# Windows NVDA Release Process

This fork publishes Windows-only builds for NVDA and screen-reader users.

## Versioning

Use fork-specific tags based on the upstream Alacritty version:

```text
v0.18.0-nvda.1
v0.18.0-nvda.2
```

The MSI package version uses the numeric upstream version portion
(`0.18.0`). The `-nvda.N` suffix is represented in the GitHub release tag and
asset names.

## Assets

The release workflow uploads:

- `Alacritty-NVDA-Windows-<tag>-portable.exe`
- `Alacritty-NVDA-Windows-<tag>-installer.msi`
- `Alacritty-NVDA-Windows-<tag>-checksums.txt`

The MSI uses a fork-specific product identity:

- Product name: `Alacritty NVDA Windows`
- Manufacturer: `MaxLogic`
- Install folder: `Program Files\Alacritty NVDA Windows`
- Start menu folder: `Alacritty NVDA Windows`
- Context menu label: `Open Alacritty NVDA here`

This identity intentionally avoids replacing or upgrading an upstream
Alacritty installation.

## Release Steps

1. Ensure `windows-nvda-accessibility` is clean and pushed.
2. Run the Windows release gate locally when practical:

   ```powershell
   cargo test --release
   cargo build --release
   ```

3. Create and push a tag:

   ```powershell
   git tag v0.18.0-nvda.1
   git push origin v0.18.0-nvda.1
   ```

4. Wait for the GitHub Actions `Release` workflow to finish.
5. Review the draft GitHub release, add user-facing notes, then publish it.

## Release Notes Checklist

Include:

- This is a Windows/NVDA compatibility fork of Alacritty.
- Main NVDA/UIA support highlights.
- Whether upstream Alacritty is installed side-by-side or separately.
- Known limitations and recommended NVDA version, if any.
- Checksums are attached as `*-checksums.txt`.
