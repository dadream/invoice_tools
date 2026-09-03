# Repository Agent Guidelines

## Build Artifacts and Disk Usage

The Cargo `target` directory contains generated build artifacts and may be
deleted and rebuilt. It must not be treated as source data or backed up as part
of the project.

- `scripts/check-build-storage.ps1` is the authoritative storage gate. Run it
  before and after build or test work; the Windows verification and portable
  build scripts invoke it automatically.
- Treat `target/debug` exceeding 20 GiB as a mandatory cleanup condition. The
  gate may run the Cargo profile-scoped cleanup automatically when
  `-AutoCleanDev` is supplied.
- Keep `target/release` at or below 12 GiB, total `target` at or below 32 GiB,
  and `artifacts` at or below 2 GiB. These are hard build gates, not targets to
  consume in ordinary work.
- Keep at least 20 GiB free before a full verification or release build and at
  least 8 GiB free after it. A build must stop rather than consume the reserve.
- Keep `ui/node_modules` below 2 GiB and the combined `.tmp`/`tmp` trees below
  2 GiB. Do not include `output` or any application `DataRoot` in automated
  build cleanup because they may contain review evidence or user-owned data.
- Before cleaning, make sure no project application, `cargo`, `rustc`, test,
  or Rust Analyzer build is running.
- Preview a development-profile cleanup first:

  ```powershell
  cargo clean --manifest-path .\Cargo.toml --profile dev --dry-run -v
  ```

- Then clean only development artifacts, preserving `target/release`:

  ```powershell
  cargo clean --manifest-path .\Cargo.toml --profile dev
  ```

- Run `cargo clean` without `--profile dev` only when removal of all generated
  profiles, including release artifacts, is explicitly intended.
- Do not manually remove individual files from `target/debug/deps`,
  `target/debug/build`, or `target/debug/incremental` while Cargo-related
  processes are active.
- Prefer package-scoped builds and tests, such as `cargo test -p <package>`,
  instead of repeatedly building the full workspace.
- Avoid unnecessary switching among feature sets, `RUSTFLAGS`, target triples,
  and toolchains, because each combination may retain a separate artifact set.

To reduce long-term disk growth, keep the following profile policy in the root
`Cargo.toml` unless full variable-level debugging or faster incremental rebuilds
are specifically required:

```toml
[profile.dev]
debug = "line-tables-only"
incremental = false

[profile.test]
debug = "line-tables-only"
incremental = false
```

This retains filenames and source line numbers for backtraces but omits
variable-level debugging. Disabling incremental compilation reduces cache
growth but makes subsequent rebuilds slower.

Historical portable candidates are generated data. The storage gate stops new
builds when `artifacts` exceeds 2 GiB. Preview retention cleanup with:

```powershell
.\scripts\prune-build-archives.ps1
```

Only after reviewing the exact `artifacts/archive` directories may the cleanup
be applied with `-Apply`. It keeps the three newest archive directories and
never touches the current candidate, evidence files, `output`, source files, or
user data.

For automated or experimental builds, use a disposable `CARGO_TARGET_DIR` on a
drive with adequate free space and clean it after the run. Relocating the target
directory does not impose a size limit.

## Package Managers

The Rust workspace uses **Cargo**. The root `Cargo.toml` defines the workspace
and `Cargo.lock` is the authoritative Rust dependency lock file.

- Run Rust build, check, and test commands with `cargo` from the repository
  root. Prefer package-scoped commands such as `cargo test -p <package>`.
- Keep `Cargo.lock` under version control. Do not replace Cargo with another
  Rust dependency manager or update dependencies merely to run validation.
- If a Cargo command needs to fetch a dependency that is not already cached,
  report the missing dependency and obtain user approval before using the
  network.

### Frontend

The `ui` project uses **npm only**. `ui/package.json` declares npm and
`ui/package-lock.json` is the authoritative dependency lock file.

- Run frontend commands with `npm --prefix ui ...` or from inside `ui` with
  `npm ...`.
- Do not use `pnpm`, `yarn`, or generate their lock files. A different package
  manager may quarantine or replace the existing npm-managed `node_modules`.
- Tests, checks, and builds must use the already installed dependencies when
  `ui/node_modules` is present. They must not trigger an install or network
  download merely to run validation.
- If dependencies are genuinely missing or the lock file requires a reinstall,
  report the reason first and obtain user approval before accessing a package
  registry. Use `npm ci` so installation remains locked to
  `ui/package-lock.json`.

## Git Commit Identity

Repository commits use the same identity as the existing project history:

```text
user.name = dadream
user.email = 285083020@qq.com
```

- Before committing, verify the latest repository commits still use this
  identity.
- Configure it in the repository-local Git config only; do not overwrite the
  user's global Git identity.
- Do not invent a different author, committer, or co-author identity.

## Git Submission Policy

This repository does not use preflight receipt commits or any equivalent
submission-receipt workflow.

- Do not run the `preflight`, `preflight-light`, or `preflight-pro` submission
  workflow for this repository.
- Do not create or modify `preflight.md`, and do not add a preflight merge
  driver or preflight commit footer.
- After the requested project checks pass, use ordinary non-interactive Git
  commands to stage, commit, push, and create release tags.
- Keep the commit identity and release gates defined above; skipping preflight
  does not waive build, test, privacy, signing, or release validation.
