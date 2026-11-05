# CI triggers and an xtask: implementation plan

This plan fixes how the workflows are triggered, so each change runs only the checks it needs and a release does not test twice. It also introduces an `xtask` for architecture validation, version bumps and release tags, and refactors the crates whose entry files hold implementation code.

## 1. Mizuoto's workflow design

`/root/code_workspace/Mizuoto` (a Python project) has three workflows.

**`ci.yml`:**
- **A `scope` job** reads the changed files: a push's from the compare API, a pull request's from its file list. `scripts/ci.py scope` maps them to areas (core, web UI, site, each gated package). Every check job runs only when its area changed.
- **Fallback:** a change to the workflows or to `ci.py`, a manual run, or a file list it cannot read checks everything.
- **Files no check reads** (README, licence, `AGENTS.md`, Docker files) check nothing.
- **Tags never start CI**, because pushes trigger it on branches only. A version tag starts Release instead.
- **Release calls CI as its checks** (`workflow_call`), so a release tests once. When a push to `main` changes a version file, CI's own run of that push skips every job, because Release is running them.
- **`CI passed`** is a last job that fails only if another job failed or was cancelled. Branch protection requires that one check, since the others may skip.
- **Concurrency:** a newer push cancels the older run of the same ref.

**`release.yml`:**
- **Triggers:** a push to `main` that changes a version file, a version tag, or a manual run with a choice of packages.
- **`plan`:** decides what to release. A tag must match its distribution's version.
- **The rest:** `checks` reuses `ci.yml`, then `build`, `smoke`, `publish` (in an approval-gated environment, skipping versions already published), and a GitHub release for tags.

**`site.yml`:**
- **Triggers:** pushes to `main` that touch `site/**` or the workflow.
- **Runs:** the site's checks and build, then the deploy to `gh-pages`. A change elsewhere deploys nothing.
- **Concurrency:** one deploy at a time.

**Tests of the tooling:**
- `tests/test_ci.py` tests the scope and release rules.
- `tests/test_architecture.py` enforces the layer rules by parsing the source.

## 2. What kardia has today, and the redundancy

| Workflow | Triggers | Jobs |
|---|---|---|
| `ci.yml` | push to `main`, pull requests (all paths) | `test`: fmt, clippy, the Rust suite (about 6 min); `site`: site checks and build |
| `publish.yml` | `v*` tags, manual | fmt, clippy, the Rust suite again, then `cargo publish --workspace` (resumable, rate-limit aware) |
| `site.yml` | push to `main` touching `site/**` | site checks, build, deploy to `gh-pages` |

The redundant runs:
1. **A docs-only change runs the Rust suite:** CI has no path filter, so it runs the suite (and the site job).
2. **A site change checks the site twice:** once in CI's `site` job and once in `site.yml`.
3. **A release runs the Rust suite twice:** the version-bump commit runs CI on its push to `main`. The `v*` tag on that same commit then runs `publish.yml`, which tests the same tree again.
4. **Nothing cancels superseded runs.**

`main` has no branch protection, so no check is required by name.

## 3. Workflow design for kardia

Two areas of the repository need different checks: **Rust** (the workspace) and **the docs site**. They become separate workflows, as in Mizuoto, and Release reuses CI's result instead of testing again.

### 3.1 `ci.yml`: Rust only

- **Triggers:** `push` to `main` and `pull_request`, both with `paths-ignore`; also `workflow_dispatch`, and `workflow_call`, so Release can run it as its checks. Tags do not trigger it.
- **Ignored paths**, which no Rust check reads:
  - `site/**`
  - `.github/workflows/site.yml`
  - `**.md`: README, `AGENTS.md`, `CLAUDE.md`, plans. The README is the facade crate's crates.io page, but no check reads it.
  - `LICENSE`, `NOTICE`, `netlify.toml`, `wrangler.toml`
- **Everything else** checks the Rust side. That includes `crates/**`, `xtask/**`, `Cargo.*`, `rustfmt.toml`, `examples/**` (the binary's example config and WASM guest) and the other workflows, so a change to CI itself is tested.
- **One job, "Rust":**
  1. `cargo fmt --check`
  2. `cargo clippy -D warnings`
  3. `cargo xtask arch` (§5)
  4. `cargo test --workspace` (pinned Rust 1.96.0, with the wasm32 target)
- **The `site` job moves to `site.yml`.**
- **Concurrency:** `ci-${{ github.workflow }}-${{ github.ref }}`, cancelling older runs of the same ref. `github.workflow` is the caller's name inside Release, so Release's call never cancels a CI run, or the other way round.

**Why path filters rather than Mizuoto's `scope` job.** Mizuoto needs a scope job because one push can concern several independently released distributions and a web UI, and it requires a single "CI passed" check in branch protection. kardia has one workspace and the site, now in separate workflows, and no branch protection. GitHub's own `paths-ignore` expresses the same rule without a job, a script or the compare API. If branch protection is added later, the scope job plus `CI passed` aggregator is the way to make a required check that may skip; the plan for that is Mizuoto's, and it would fit here unchanged.

### 3.2 `site.yml`: the docs site only

- **Triggers:**
  - `push` to `main` touching `site/**` or the workflow: check, build and deploy.
  - `pull_request` touching `site/**`: check and build only.
  - `workflow_dispatch`.
- **One job:**
  1. `bun install --frozen-lockfile`
  2. `check`, `test`, then `build` with `VITE_BASE_PATH=/kardia/`
  3. The deploy steps (commit `site/.output/public` and force-push `gh-pages` with `GITEE_RSA_PRIVATE_KEY`). These run only outside pull requests, so each change builds the site once.
- **Concurrency:** deploys run one at a time and never cancel each other (`gh-pages` is force-pushed). Pull-request runs of the same branch cancel older ones.

### 3.3 `release.yml` (replaces `publish.yml`)

- **Triggers:** `push` of a `v*` tag; `workflow_dispatch`, which resumes a release from the chosen ref.
- **`plan`:** reads the workspace version (`cargo metadata`). For a tag, it fails unless the tag is `v<version>`.
- **`ci-result`:** finds CI's run for the released commit. It's a `push` or `workflow_dispatch` run of `ci.yml` with the same SHA.
  - **Still running:** it waits for it (`gh run watch --exit-status`).
  - **Succeeded:** the release reuses that result.
  - **Failed:** the release stops.
  - **None, or cancelled** (a commit CI never ran on, or a run cancelled by a newer push): the next job runs the checks.
- **`checks`:** `uses: ./.github/workflows/ci.yml`. It runs only when `ci-result` found no usable run, so each commit is tested at most once.
- **`publish`:** the existing resumable `cargo publish --workspace`, which skips versions already on crates.io and waits out the new-crate rate limit. It needs `plan`, plus `ci-result` succeeding or `checks` succeeding.
- **`github-release`** (tags only): `gh release create <tag> --generate-notes --verify-tag`, with notes since the previous `v*` tag. This needs `contents: write`.

### 3.4 What each kind of change runs

| Change | Runs |
|---|---|
| A docs page (`site/**`) | `site.yml` only (on `main`: build and deploy) |
| README / `AGENTS.md` / licence | nothing |
| Rust code, tests, `Cargo.*`, `xtask` | `ci.yml` |
| A workflow other than `site.yml` | `ci.yml` |
| A version bump commit, then its tag | `ci.yml` once (the push); `release.yml` waits for that run and publishes, without testing again |
| A tag on a commit CI never tested | `release.yml` runs `ci.yml` as its checks, then publishes |

## 4. The xtask

An `xtask` crate at the repository root, following the cargo-xtask convention. It's a workspace member (`members = ["crates/*", "xtask"]`) with `publish = false`, so it is never published. `.cargo/config.toml` adds the alias `xtask = "run --package xtask --"`, so commands run as `cargo xtask <command>`.

**Layout** (the binary's `main.rs` only dispatches):

```text
xtask/
  Cargo.toml
  src/
    main.rs      # argument parsing and dispatch
    arch.rs      # cargo xtask arch
    version.rs   # cargo xtask version
    tag.rs       # cargo xtask tag
    git.rs       # running git, shared by version and tag
```

**Dependencies:**
- `syn` (full parsing) and `proc-macro2` with `span-locations`, for line numbers. Both are already workspace dependencies.
- `toml_edit`: rewrites `Cargo.toml` while keeping its formatting and comments.
- Arguments are parsed by hand, since there are three commands with a few flags each.

### 4.1 `cargo xtask arch`: architecture validation

**The rule.** An entry file is a module's front door. It holds:
- module declarations,
- `use` / `pub use` re-exports,
- crate or module docs and attributes.

All implementation lives in a file of its own.

**Entry files checked:** every `lib.rs` and `mod.rs` under `crates/*/src`, `crates/*/tests` and `xtask/src`. Test fixtures that are separate crates (`crates/kardia-hmr/tests/fixtures/*`) are left out.

**Items allowed in an entry file:**
- **Module declarations:** `mod x;` / `pub mod x;`.
- **Inline modules** (`pub mod prelude { … }`), whose contents follow the same rule recursively. They can only re-export.
- **Re-exports:** `use` / `pub use`.
- **External crates:** `extern crate`.
- **Proc-macro entry points:** functions marked `#[proc_macro]`, `#[proc_macro_derive]` or `#[proc_macro_attribute]`. Rust requires them at the crate root; they stay thin wrappers that call into their module.

**Reported:** any other item, with its file, line and kind (`fn`, `struct`, `enum`, `trait`, `impl`, `type`, `const`, `static`, `macro_rules!`, `union`), and a hint to move it into its own module. An inline `#[cfg(test)] mod tests { … }` is reported too: tests belong next to the code they test. The command exits 1 when anything is reported, and CI runs it.

### 4.2 Refactoring so the rule holds

| Entry file | Now holds | Moves to |
|---|---|---|
| `kardia/src/lib.rs` | `catalog()` | `catalog.rs`; `prelude` stays inline (re-exports only) |
| `kardia-loader/src/lib.rs` | `builtins()` | `catalog.rs`, next to `Catalog` |
| `kardia-fs/src/lib.rs` (320 lines) | `Fs`, `FsPlugin`, `WatchOptions`, `FsExt`; `Watcher`, `Subscription`, the dispatch internals and their tests | `service.rs` (service, plugin, options, extension trait); `watcher.rs` (watcher, its internals and tests) |
| `kardia-timer/src/lib.rs` (318) | `Timer`, `TimerPlugin`, `Disposed`, `TimerExt`; `Ticks`; `Scheduled` and `schedule`; `TimerContext` | `service.rs`; `ticks.rs`; `schedule.rs`; `context.rs` |
| `kardia-logger-console/src/lib.rs` (220) | config types; `ConsoleExporter`; `ConsolePlugin`; colour and duration helpers | `config.rs`; `exporter.rs`; `plugin.rs`; `format.rs` |
| `kardia-guest/src/lib.rs` (272) | `Guest`, `Request`; `Meta`, `PluginMeta`; `Error`, `Result`; `Level` and the public `Host` methods; `export!` | `guest.rs`; `meta.rs`; `error.rs`; `host.rs`; `export.rs` (the macro, still `#[macro_export]`) |
| `kardia-macros/src/lib.rs` (221) | the two entry points and their expansions | `service.rs` (`derive(Service)`); `plugin.rs` (`#[plugin]`); `util.rs` (case conversion, type helpers). The entry points stay in `lib.rs` and delegate. |
| `tests/common/mod.rs` in core, loader, include, hmr (66–252 lines) | test helpers | Files by topic under `tests/common/` (for example `gate.rs`, `mock.rs`, `app.rs`), with `mod.rs` declaring and re-exporting them so test files keep their imports |

**What doesn't change:**
- Public paths: everything is re-exported from where it is today, so `kardia_fs::Watcher`, `kardia_timer::TimerExt` and the rest stay the same.
- Behaviour.
- The crates' public docs; crate-level docs stay in `lib.rs`.

Items that were private to one file and are now used across files become `pub(crate)`.

### 4.3 `cargo xtask version <patch|minor|major>`

Reads the workspace version, computes the next one, and updates every place that names it:
1. **The root `Cargo.toml`:**
   - `[workspace.package] version`, which every crate inherits.
   - The `version` of each `kardia-*` entry in `[workspace.dependencies]`, so the published crates require each other's new version.
2. **`Cargo.lock`:** `cargo update --workspace --offline` records the workspace crates' new versions.
3. **Dependency snippets** in `README.md` and `site/content/docs/*.mdx` (`kardia = "0.1"`, `kardia-guest = "0.1"`), when the requirement a user writes changes:
   - before 1.0, a minor bump (`0.1` → `0.2`) or a major bump;
   - after 1.0, a major bump.
   A patch bump leaves them alone.

**Semantic versioning:**
- `patch`: `0.1.0` → `0.1.1`
- `minor`: `0.1.3` → `0.2.0`
- `major`: `0.4.2` → `1.0.0`

**Behaviour:**
- It prints each changed file and the old → new version.
- `--dry-run` prints the changes without writing.
- It refuses a pre-release or otherwise unparsable current version.
- It does not commit; committing stays a deliberate step.

### 4.4 `cargo xtask tag`

Creates an annotated release tag on `HEAD`:
- **The tag:**
  - `cargo xtask tag` tags `v<workspace version>`.
  - `--version X.Y.Z` tags `vX.Y.Z`, and refuses unless it equals the workspace version: a `v*` tag publishes the workspace, and Release would reject the mismatch.
  - `--name <tag>` tags any other name (not a release tag).
- **Flags:**
  - `--message <text>` sets the annotation (default `kardia <version>`).
  - `--push` pushes the new tag to the remote. `--remote <name>` picks the remote (default `origin`).
  - `--dry-run` prints what would happen.
- **Refused:**
  - an uncommitted change in the working tree (the tag must name the committed version);
  - a tag that already exists locally, or on the remote when pushing.
- **Release flow:**

  ```sh
  cargo xtask version patch
  git commit -am "chore(release): kardia 0.1.1"
  git push
  cargo xtask tag --push
  ```

  The push of the commit runs CI once. The tag starts Release, which waits for that run and publishes.

## 5. Testing strategy

**xtask unit tests,** pure functions on inputs, no mocks:
- **Version arithmetic:** patch, minor and major; the requirement rule before and after 1.0; refusing a pre-release.
- **Manifest rewriting:** a sample root `Cargo.toml`, checking that the package version and the `kardia-*` dependency versions change, non-kardia entries and comments survive, and the snippet rewriting covers both `kardia` and `kardia-guest`.
- **Architecture rules:** sample sources covering every allowed item (declarations, re-exports, docs, an inline re-export module, a proc-macro entry point) and every reported kind (including an inline `mod tests`), with the reported lines.
- **Tag naming and validation.**

**xtask integration tests** against real git:
- A temporary repository with a bare remote.
- `tag` creates the annotated tag, refuses a dirty tree, an existing tag or a version mismatch, and `--push` lands it on the remote.
- `version` on a copy of a small workspace updates its files.

**On the repository:**
- `cargo xtask arch` must pass after the refactor, and CI runs it.
- The whole suite (483 tests) passes unchanged, since public paths do not move.
- `cargo doc` and the site build pass.

**The workflows**, by real runs after the change:
- **A docs-only commit:** only `site.yml` runs.
- **A Rust commit:** only `ci.yml` runs.
- **Release:** a manual run of `release.yml` on `main`. Every version is already published, so it publishes nothing, but it exercises the version check, finding and reusing CI's run of that commit, and the publish step's skip logic.

## 6. Repository changes

- **New:** `xtask/` (crate), `.cargo/config.toml` (alias), `.github/workflows/release.yml`.
- **Removed:** `.github/workflows/publish.yml`.
- **Changed:**
  - `.github/workflows/ci.yml`: Rust only, path filters, `workflow_call`, `cargo xtask arch`, concurrency.
  - `.github/workflows/site.yml`: pull-request checks, concurrency.
  - The root `Cargo.toml`: workspace members, and `toml_edit` for xtask.
- **New module files** in `kardia`, `kardia-loader`, `kardia-fs`, `kardia-timer`, `kardia-logger-console`, `kardia-guest`, `kardia-macros`, and the four `tests/common/` directories (§4.2).
- **Docs:** the site's Contributing page (both languages) and the README's Development section describe the workflows, the xtask commands and the release flow. `AGENTS.md` gains the entry-file rule.

## 7. Order of work

Each step is a commit; the workflows are pushed last, after everything they run passes locally.
1. This plan.
2. The `xtask` crate with `arch`, `version` and `tag`, and their tests.
3. The refactor, until `cargo xtask arch` passes; the full suite stays green.
4. The workflows: `ci.yml`, `site.yml`, `release.yml`.
5. The docs.
6. Push, then verify each workflow by real runs (§5).
