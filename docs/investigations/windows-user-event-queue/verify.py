"""Offline RED/GREEN proof against the exact cached winit crate and retained patch."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def main():
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--winit-source", type=Path)
    parser.add_argument("--output", type=Path, default=here / "target/proof")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    metadata = json.loads((here / "candidate.json").read_text())
    candidates = list((Path.home() / ".cargo/registry/src").glob("*/winit-0.30.13"))
    source = args.winit_source or (candidates[0] if len(candidates) == 1 else None)
    if source is None:
        parser.error("specify --winit-source pointing at an unpacked winit 0.30.13 crate")
    relative = Path("src/platform_impl/windows/event_loop.rs")
    digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
    assert digest(source / relative) == metadata["original_file_sha256"], "unexpected upstream file"
    patch = here / "winit-0.30.13-pending-events.patch"
    assert digest(patch) == metadata["patch_sha256"], "patch provenance mismatch"
    patched = output / "winit-0.30.13"
    if patched.exists():
        assert digest(patched / relative) == metadata["candidate_file_sha256"], "unexpected existing candidate"
    else:
        shutil.copytree(source, patched)
        subprocess.run(["git", "apply", "--check", str(patch)], cwd=patched, check=True)
        subprocess.run(["git", "apply", str(patch)], cwd=patched, check=True)
        assert digest(patched / relative) == metadata["candidate_file_sha256"], "patched content mismatch"
    results = []

    def run(name, command, expected):
        with (output / f"{name}.log").open("w", encoding="utf-8") as log:
            completed = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=180)
        results.append({"name": name, "exit_code": completed.returncode, "expected": expected})
        (output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        print(f"{name}: exit {completed.returncode} (expected {expected})", flush=True)
        assert completed.returncode == expected, f"inspect {output / (name + '.log')}"
        if name in {"baseline-burst", "baseline-foreign"}:
            log_text = (output / f"{name}.log").read_text()
            delivered = 10000 if name == "baseline-burst" else 0
            assert f"accepted: 20000; delivered: {delivered};" in log_text, "unexpected baseline failure"
            assert "accepted events must reach the application" in log_text

    shutil.copytree(here / "repro", output / "repro", dirs_exist_ok=True)
    manifest = output / "repro/Cargo.toml"
    vendor = here.parents[2] / "vendor/winit"
    if vendor.is_dir():
        hashes = json.loads((vendor.parent / "winit-files.sha256.json").read_text())
        actual = {p.relative_to(vendor).as_posix(): digest(p)
                  for p in vendor.rglob("*") if p.is_file()}
        assert actual == hashes, "vendored crate provenance mismatch"
        assert digest(vendor / relative) == metadata["candidate_file_sha256"]
        patched = vendor
    for kind in ["baseline", "candidate"]:
        target = output / f"target-{kind}"
        build = ["cargo", "build", "--offline", "--manifest-path", str(manifest), "--target-dir", str(target)]
        if kind == "candidate":
            build += ["--config", f'patch.crates-io.winit.path="{patched.as_posix()}"']
        run(f"{kind}-build", build, 0)
        modes = ["burst", "foreign"] if kind == "baseline" else [
            "burst", "foreign", "race", "prestart", "exit", "foreign-exit", "pump-foreign", "pump-race", "pump-exit"]
        exe = target / "debug/winit-wake-repro.exe"
        for mode in modes:
            run(f"{kind}-{mode}", [str(exe), mode], 101 if kind == "baseline" else 0)
    print(f"Complete; proof retained in {output}")


if __name__ == "__main__":
    main()
