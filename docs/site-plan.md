# Documentation site: implementation plan

This plan moves kardia's documentation from the `docs/` directory to a documentation website, built with the same framework as [rvc-next](https://github.com/licyk/rvc-next)'s site. It covers the architecture, the files that change, the migration of every existing document, the Giscus and Umami settings, the build, and the release workflow.

## 1. What rvc-next's site is

`rvc-next/site/` is a self-contained web project. Nothing outside `site/` is needed to build it.

| Part | What it is |
|---|---|
| Stack | [Fumadocs](https://fumadocs.dev) (`fumadocs-core`, `fumadocs-ui`, `fumadocs-mdx`) on TanStack Start (React 19), built with Vite 8 and Tailwind CSS 4. Bun is the package manager and script runner (`bun.lock`). |
| Output | A static site. TanStack Start runs in SPA mode with prerendering and link crawling, so every page is an HTML file in `site/.output/public/`. `scripts/prepare-static-output.mjs` checks that every page in `content/docs/meta.json` was prerendered, and copies the SPA shell to `404.html`. |
| Base path | `vite.config.ts` reads `VITE_BASE_PATH`. GitHub Pages serves the site under `/<repo>/`; other hosts serve it at `/`. |
| Content | MDX files in `content/docs/`, flat slugs grouped by prefix (`guide-*`, `development-*`). The sidebar order and its section headings come from `meta.json`. |
| Languages | Chinese (`zh-CN`) is the default and has no URL prefix; English pages are `*.en.mdx` under `/en/…`, with their own `meta.en.json`. A test requires an English page for every Chinese one, and the same slugs in the same order in both sidebars. A switch in the header changes the language, remembered in `localStorage`. |
| Pages | `/`: a home page of five sections (hero, features, workflow, highlights, install) with a section rail and scroll animations, its text in `src/lib/site-copy.ts`. `/docs/…`: the docs layout with sidebar, table of contents, full-text search (`/api/search`, a prerendered static index) and a comment thread per page. |
| MDX components | Fumadocs' defaults plus `Cards`, `Callout`, `Steps`, `Tabs`, `Accordions` and a `Mermaid` diagram component. |
| Comments | Giscus, mounted by `src/components/docs-comments.tsx` from attributes in `src/lib/giscus.ts`. rvc-next maps each page to a normalized term (`data-mapping="specific"`), so the base path, the language prefix and trailing slashes do not split a thread, and it posts the site's theme and language to the frame. |
| Analytics | Umami, one `<script>` in the root route's head (`src/routes/__root.tsx`): `https://licyk-umami.netlify.app/script.js` with a `data-website-id`. |
| Checks | `bun run check` (TypeScript), `bun run test` (Vitest: i18n, content parity, Giscus terms, theme, home sections, motion), `bun run build`. |
| Hosting | `.github/workflows/site.yml` builds with `VITE_BASE_PATH=/rvc-next/` and force-pushes `site/.output/public` to the `gh-pages` branch over SSH with the `GITEE_RSA_PRIVATE_KEY` secret. GitHub Pages serves that branch. `netlify.toml`, `wrangler.toml` (Cloudflare Pages), `site/vercel.json` and `public/_redirects` let those hosts build the same site at the root path; each needs connecting in its own dashboard. rvc-next's CI also type-checks, tests and builds the site on every change. |

## 2. What kardia has today

- **Documentation:** `docs/` holds 21 Markdown files, about 2,650 lines, all in English:
  - `docs/README.md`: the index.
  - `docs/paper/01–08`: the paper's theory.
  - `docs/implementation/01–11`: an analysis of the TypeScript Cordis code.
  - `docs/rust-implementation-plan.md`: the Rust design, phases, testing strategy, risks, and the implementation status in §15.

  The root `README.md` is the user guide.
- **Build:** a Cargo workspace. There is no JavaScript tooling yet; Bun 1.4.2 and Node 24 are available on this machine.
- **CI:** `.github/workflows/ci.yml` (fmt, clippy, tests on Rust 1.96.0) and `publish.yml` (crates.io on `v*` tags).
- **Secrets:** `CARGO_REGISTRY_TOKEN` and `GITEE_RSA_PRIVATE_KEY`. The latter is the deploy key rvc-next's site workflow uses, so the docs workflow reuses it and no new secret is needed.
- **GitHub:** the repo is public. Discussions are enabled, and the "Comment" category (`DIC_kwDOVjB9NM4DKbXu`) matches the Giscus configuration. GitHub Pages is not enabled yet.
- **Links into `docs/`:** the README, `NOTICE`, and three test comments (`plan §6.4`, `§6.5`, `§11.2` in `kardia-core/tests/{shadow,associate,model}.rs`).

## 3. Architecture

A new `site/` directory, copied from rvc-next's `site/` and adapted.

**Copied unchanged:** the framework parts.
- Build and type configs: `vite.config.ts`, `tsconfig.json`, `vitest.config.ts`.
- `scripts/prepare-static-output.mjs` and `public/{.nojekyll,_redirects}`.
- Routing: `src/router.tsx` and the routes `spa-shell`, `api/search` and `docs/$`.
- Components: `mdx`, `mermaid`, `search`, `site-provider`, `theme-toggle`, `not-found`, `page/*`.
- Libraries: `src/lib/{source,i18n,theme-mode,page-motion,document-metadata,home-sections}.ts` and their tests.
- `src/styles/*`.

**Adapted:**
- `package.json`: renamed `@kardia/site`.
- `src/lib/layout.shared.tsx`: title "kardia" and the repository URL.
- `src/lib/i18n.ts`: the locale storage key becomes `kardia.site.locale`.
- `src/routes/__root.tsx`: title, description, Umami ID (§7).
- `src/routes/docs/$.tsx`: page titles.
- `src/lib/giscus.ts` and `src/components/docs-comments.tsx`: the new configuration (§6).
- `src/assets/favicon.svg`: a kardia mark.

**Rewritten for kardia:**
- The home page text in `src/lib/site-copy.ts`.
- The product preview in the hero: rvc-next shows its app's interface. kardia instead shows a plugin in Rust next to the live log of a hot reload, built from the same card styles.
- `src/routeTree.gen.ts` is generated by TanStack Router and is regenerated by the build, not edited.

**Home page sections**, keeping the five-section structure:
1. **Hero:** "Plugins that clean up after themselves", with links to Quick start and Learn.
2. **Features:**
   - revertible effects
   - reactive services
   - live reconfiguration from a config file
   - hot reload of guest plugins (WASM, dylib, process)
   - checked dependencies at compile time
3. **Workflow:**
   1. Install: `cargo add kardia` or `cargo install kardia`.
   2. Write a plugin.
   3. Describe the app in `kardia.yml`.
   4. Edit while it runs.
4. **Highlights:** a faithful port of Cordis, 481 tests including the ported TypeScript suite, property tests of the paper's theorems, GPL-3.0.
5. **Install:** links to the Installation page.

**Hosting files,** copied with the names changed:
- `netlify.toml` and `wrangler.toml` at the root, and `site/vercel.json`.
- These hosts still need connecting in their own dashboards. GitHub Pages is the published site.

## 4. Documentation migration

`docs/` is replaced by `site/content/docs/`. Every existing document maps to a page; none is dropped. Pages are rewritten for a reader of a website: shorter sections, tables and diagrams where they help, and cross-links instead of "see §n.n". All technical content, decisions and deviations are kept. Where a section is split, every paragraph of the original lands on exactly one page, and a coverage check (§9) confirms it.

| Section (sidebar) | Page slug | Source |
|---|---|---|
| **Getting started** | `index` | `README.md` intro and contents, `docs/README.md` |
| | `quick-start` | `README.md` Quick start |
| | `installation` | `README.md` Requirements, crates.io install, the WASM target |
| **Guide** | `guide` | Overview of the guide (new, linking the pages below) |
| | `guide-configuration` | `README.md` The configuration file: entry fields, expressions, two-way sync, built-in plugins |
| | `guide-plugins` | `README.md` Writing plugins in Rust; plan §6.1–6.3 (the `Plugin` trait, `PluginDef`, `#[plugin]`) |
| | `guide-services` | Plan §6.4–6.5, §8 (services, `Svc`, checked access, interception, events, logger) |
| | `guide-effects` | Plan §7 (effects, disposers, `EffectScope`, disposal) |
| | `guide-loader` | Plan §10 (loader, groups, isolation, include, catalog) |
| | `guide-hot-reload` | `README.md` hot reload; plan §10.3 (HMR, watch mode) |
| | `guide-guests` | `README.md` guest plugins; the guest ABI from the `kardia-hmr` crate docs (WASM, dylib, process transport) |
| **Concepts** (the paper) | `concepts` | `docs/paper/01` (overview, contributions, glossary) |
| | `concepts-revertible-effects` | `docs/paper/02` |
| | `concepts-reactive-coeffects` | `docs/paper/03` |
| | `concepts-context-paradigm` | `docs/paper/04` |
| | `concepts-calculus` | `docs/paper/05` |
| | `concepts-metatheory` | `docs/paper/06` |
| | `concepts-implementation` | `docs/paper/07` |
| | `concepts-related-work` | `docs/paper/08` |
| **Cordis reference** (the TS code) | `reference` | `docs/implementation/01` |
| | `reference-context` | `docs/implementation/02` |
| | `reference-effects` | `docs/implementation/03` |
| | `reference-fibers` | `docs/implementation/04` |
| | `reference-services` | `docs/implementation/05` |
| | `reference-events` | `docs/implementation/06` |
| | `reference-loader` | `docs/implementation/07` |
| | `reference-include` | `docs/implementation/08` |
| | `reference-hmr` | `docs/implementation/09` |
| | `reference-auxiliary` | `docs/implementation/10` |
| | `reference-paper-vs-code` | `docs/implementation/11` |
| **Development** (the Rust design) | `development` | Plan header, §1 (goals), §2 (concept mapping), §3 (crate layout, dependencies) |
| | `development-concurrency` | Plan §4 (execution model, the single lock, actions) |
| | `development-lifecycle` | Plan §5 (state, fibers, targets, inertia, guards) |
| | `development-design` | Plan §6–9 (API design: plugins, services, effects, events, logger) |
| | `development-loader` | Plan §10 (loader, include, HMR internals) |
| | `development-testing` | Plan §11 (spec ports, property tests, runtimes) |
| | `development-status` | Plan §12 (phases), §15 (status and every deviation) |
| | `development-risks` | Plan §13–14 (risks, open decisions) |
| | `development-contributing` | `README.md` Development; CI, releases, the docs site |

The old `docs/` directory is then removed. Its contents live on the site, and git history keeps the originals. This plan file, a working document, goes with it; the docs site itself is described on `development-contributing`. The README links to the site, and the three test comments point to `development-design` and `development-testing` instead of plan sections.

## 5. Languages

The framework is bilingual: Chinese by default, plus English. Its tests require every page in both languages. kardia's documents are all in English today.

**Recommended:** keep the framework's model.
- Chinese is the default language, as in rvc-next and as the Giscus language `zh-CN` suggests.
- English pages are kept alongside.
- English pages are rewritten from the sources first; the Chinese pages follow, translated from them.
- Code, identifiers, configuration examples and the paper's notation stay as they are.

This doubles the pages to about 80 MDX files.

**Alternative:** English only.
- Drop the i18n layer, the language switch and the parity test.
- Less work, but it diverges from rvc-next's framework, and the Giscus language would not match the site.

## 6. Giscus

Decided: the given repository and category, with rvc-next's comment behaviour.

| Attribute | Value |
|---|---|
| `data-repo` | `licyk/kardia` |
| `data-repo-id` | `R_kgDOVjB9NA` |
| `data-category` | `Comment` |
| `data-category-id` | `DIC_kwDOVjB9NM4DKbXu` |
| `data-strict` | `1` |
| `data-reactions-enabled` | `1` |
| `data-emit-metadata` | `0` |
| `data-input-position` | `top` |
| `data-loading` | `lazy` |
| Script | `https://giscus.app/client.js`, `crossorigin="anonymous"`, `async` |

Kept from rvc-next:
- **One thread per page:** `data-mapping="specific"` with a `data-term` per page instead of `pathname`. The term drops the `/kardia/` base path, the `/en` language prefix and surrounding slashes, so the Chinese and English versions of a page share one thread, on every host.
- **Theme and language follow the site:** `data-theme` and `data-lang` follow the theme toggle and language switch, and changes are posted to the loaded frame.

All fixed attributes live in `GISCUS_ATTRIBUTES` in `src/lib/giscus.ts`; tests cover the attributes and the terms.

## 7. Umami

The only place Umami is initialized is the `scripts` entry of the root route's head (`src/routes/__root.tsx`). The new website ID `0fbf20c3-e35e-4017-a3de-62b51a57cb55` goes there, keeping the same script URL (`https://licyk-umami.netlify.app/script.js`). A test asserts the ID, and a search of `site/` after the build confirms that no other website ID remains.

## 8. Build and integration

- **Local:**

  ```sh
  cd site
  bun install
  bun run dev       # local server
  bun run check     # TypeScript
  bun run test      # Vitest
  bun run build     # static output in site/.output/public
  ```

  `bun.lock` is committed, so CI installs with `--frozen-lockfile`.
- **Ignore rules:** `site/.gitignore` (copied) covers `node_modules`, `.output`, `.nitro`, `.tanstack`, `.source` and `dist`. The root `.gitignore` is unchanged, since `/cordis/` and `target/` don't overlap with these.
- **Cargo:** the site is outside the Cargo workspace (`members = ["crates/*"]`), so `cargo` commands and the crates.io packages are unaffected.
- **CI** (`ci.yml`): a second job, "Documentation site", runs `bun install --frozen-lockfile`, `check`, `test` and `build` on every push and pull request, as rvc-next's CI does.
- **README:** links to the site and to the relevant pages instead of `docs/`. `NOTICE` is unchanged.

## 9. Release workflow

`.github/workflows/site.yml`, adapted from rvc-next's.
- **Triggers:** pushes to `main` that touch `site/**` or the workflow itself, plus `workflow_dispatch`.
- **Steps:**
  1. Check out the repo and set up Bun.
  2. `bun install --frozen-lockfile`, then `check`, `test`, and `build` with `VITE_BASE_PATH=/kardia/`.
  3. Commit `site/.output/public` into a fresh repository, and force-push it to the `gh-pages` branch of `git@github.com:licyk/kardia.git` with the existing `GITEE_RSA_PRIVATE_KEY` secret.

After the first deployment, GitHub Pages is enabled on the `gh-pages` branch (`/` path), as in rvc-next. That's a one-time repository setting, made through the GitHub API. The site is then at **https://licyk.github.io/kardia/**.

**Verification:**
- **Locally:**
  - `bun run check`, `bun run test` and `bun run build` all pass.
  - The prerendered output contains every page in both languages.
  - The search index lists them.
  - A local preview of the built site serves pages, search, the language switch, Mermaid diagrams and the comment frame.
- **Coverage check:** a script lists every heading and table of the old `docs/` and the README's user-guide sections, and confirms each appears on a page.
- **After deploying:**
  - The workflow succeeds.
  - `https://licyk.github.io/kardia/` and a deep link like `/kardia/docs/guide-plugins` answer 200.
  - The Umami script carries the new ID.
  - A docs page loads the Giscus frame with the configured attributes.

## 10. Order of work

Each step is a commit.

1. **This plan.**
2. **The framework:** copy `site/`, adapt names, Giscus and Umami, with a placeholder home page. Tests and build pass.
3. **The kardia home page:** copy and visuals.
4. **The content:** English pages, then Chinese pages, section by section, with the coverage check.
5. **Cleanup:** remove `docs/`, and update the README, the test comments, and CI's site job.
6. **The release workflow:** deploy, enable GitHub Pages, verify the live site.

## 11. Decisions

Both decided:
1. **Languages** (§5): bilingual, Chinese by default, English alongside.
2. **Giscus** (§6): the given repository and category, with rvc-next's per-page terms and theme and language sync.
