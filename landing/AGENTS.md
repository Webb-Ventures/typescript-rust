# landing

The ts-r.dev website, all of it, built with Next.js (App Router) and exported as static files
(`output: 'export'`, `trailingSlash: true`): the home page, `/tools/` and one page per tool
(`/tools/<name>/`), `/donate/` (sponsor tiers), 404 and, once launched, the docs (`/docs/…`),
one page per error code (`/errors/<CODE>/`), the benchmarks dashboard (`/benchmarks/`), search
(`/search/`), a Markdown copy of every docs and error page (`index.md` next to its
`index.html`), `llms.txt` and the raw data (`/data/*.json`). Root rules:
[../AGENTS.md](../AGENTS.md) when present.

## Modes

`mode` in `data/site.json` (or the `SITE_MODE` repository variable, which wins). One build
serves both; the mode only decides what is published and linked:

- `coming-soon`: home, tools, donate, 404, sitemap and robots.
- `launched`: also the docs, error pages, benchmarks, search, Markdown copies, `llms.txt` and
  `/data/`. The header and footer gain Docs, Benchmarks, Error codes and GitHub; the sitemap
  lists the docs, error and benchmark pages.

Launched-only routes are files named `page.launched.tsx` / `route.launched.ts`. `next.config.ts`
adds those extensions to `pageExtensions` only in launched mode, so in coming-soon mode the
routes don't exist and nothing of them is exported (`scripts/check.mjs` fails if `out/` has
them). Put any new launched-only route in such a file.

Deploy: `.github/workflows/pages.yml` (GitHub-hosted runners; pull requests build, pushes to
main deploy) runs `npm ci` and `npm run build` here and uploads `out/`. Pull requests from
authors in the `PREVIEW_AUTHORS` repository variable (default `riley-wv`) also deploy that
export to a Cloudflare Worker at `https://preview-pr-<n>.ts-r.dev/` (noindex);
`.github/workflows/preview-cleanup.yml` deletes it when the PR closes. The site is served from a
domain root in both places, so it needs no base path. `SITE_URL` (default `https://ts-r.dev`)
sets absolute URLs (canonical, sitemap, Markdown copies, `llms.txt`).

Domains: `ts-r.dev` is the site (custom domain in Settings → Pages). GitHub Pages serves one
domain per repo, so `tsrust.dev` redirects to `https://ts-r.dev/` at the DNS provider (a 301
that keeps the path).

## Commands

```sh
npm ci
npm run build                     # next build, then scripts/check.mjs (below); writes out/
SITE_MODE=launched npm run build  # the launched site, whatever data/site.json says
npm run check                     # the checks alone (sources, and out/ if it exists)
npm run dev                       # next dev (launched routes need SITE_MODE=launched)

cargo xtask diag-codes --out landing/data/diagnostics.json   # after a crate adds or drops a code
node scripts/verify-examples.mjs [CODE...]                    # after editing an error example
```

Node 22.18 or later: `scripts/check.mjs` imports `lib/tools.ts` with Node's type stripping. Run
`npm run build` through `../scripts/limit.sh` in the toolchain repo.

`scripts/check.mjs` fails, listing every problem, when:

- a source in `content/` is invalid (front matter, an error code missing from
  `data/diagnostics.json`, an unverified example, a relative link to a missing repo file), or
  `data/bench.json` has a chart without a complete method;
- a quoted number left its source: a `data/conformance.json` row whose `quote` is no longer in
  its `source` file (a crate's `AGENTS.md` or a design record);
- `lib/tools.ts` fell out of step with `content/tools/`: a tool without a docs page (or the
  reverse), a `usage` whose first line differs from the docs page's `--help` block or that shows
  a flag the block doesn't list, a `sections` id missing from bench.json, or a `correctness` row
  with no `"home"` label in `data/conformance.json`;
- `out/` has a broken internal link or `#anchor` (every `href`/`src` in the HTML and every
  `https://ts-r.dev/…` URL in HTML, XML, text and Markdown; external links are counted, not
  fetched), or a launched-only page in a coming-soon build.

The pages themselves refuse to build while a content problem exists (`assertContent()`), so a
broken source can't be published even without the check.

## Files

| Path | What it is |
|---|---|
| `app/` | Routes. `*.launched.tsx` / `*.launched.ts` exist only in launched mode |
| `app/globals.css`, `app/docs.css` | The only stylesheets: the home and tool pages, then the reading layout (docs, errors, search, benchmarks) |
| `components/` | Page parts: `parts.tsx` (header, footer, tiles), `charts.tsx` (every chart, from bench.json), `docs.tsx` and `doc-page.tsx` (the docs shell), `search.tsx` (client-side search) |
| `content/*.md`, `content/tools/*.md` | Docs pages, rendered at `/docs/<name>/` (`index.md` is `/docs/`) |
| `content/errors/<CODE>.md` | Error-code pages, rendered at `/errors/<CODE>/` |
| `lib/content/docs.mjs` (+ `docs.d.mts`) | Loads and renders `content/`: pages, nav, search index, Markdown copies, `llms.txt`, the status page's generated tables; collects every problem |
| `lib/content/markdown.mjs` | Our Markdown subset: front matter, headings with anchors, lists, tables, fences, blockquotes, inline HTML |
| `lib/content/examples.mjs` | Error-page example blocks, shared by the pages and `scripts/verify-examples.mjs` |
| `lib/bench.ts`, `lib/tools.ts`, `lib/site.ts` | bench.json access, each tool's copy, site config and mode |
| `scripts/check.mjs` | The checks above |
| `scripts/verify-examples.mjs` | Runs the error-page examples through `ts-r-check` and records the output |
| `data/site.json` | Mode, the Stripe donate link (a choose-what-to-pay Payment Link), sponsor tiers and sponsors. Each tier's button opens the link with `?prefilled_amount=<cents>`; a tier with its own `url` uses that. `orMore` marks the open-ended top tier |
| `data/bench.json` | Benchmark data and method notes, the only copy; also published at `/data/bench.json` |
| `data/conformance.json` | Correctness rows: the status page's table, and (rows with `home`) the home and tool pages'. Each row quotes its source file |
| `data/diagnostics.json` | Every code ts-r can report, with tsc's message. Generated: `cargo xtask diag-codes --out landing/data/diagnostics.json` |
| `data/error-examples.json` | What `ts-r-check` printed for each error-page example. Generated: `node scripts/verify-examples.mjs` |
| `data/error-selection.json` | How the error codes with pages were chosen, and the counts behind it |
| `public/` | Copied as is: `.nojekyll`, `favicon.svg`, `apple-touch-icon.png` and `assets/og-image.*` (kept so the docs site's old icon and share-image URLs still resolve) |

Dependencies: `next`, `react`, `react-dom` and `posthog-js`; dev: `typescript`, the React and
Node types, `@fontsource/inter` (the share image's font). The Markdown renderer is our own
(`lib/content/markdown.mjs`, about 300 lines): the docs use a small subset, and keeping it means
the heading anchors, tables and example blocks render exactly as before. Add a dependency only
when it saves real work, and say why here.

## The rule for numbers

**Every number on the site cites how it was measured.** That means: what was measured, the
machine, the software versions, the input (project or generated app, and its size), the number of
runs, and the commit or file where the result is recorded. If you can't say all of that, don't
publish the number; if the source doesn't record one of them (machine, runs), say so.

- Never invent, round up or extrapolate a result. Copy it from a commit message, a crate's
  `AGENTS.md`, a design record or a benchmark output you ran, and link that source.
- Targets are not measurements. Say "target" wherever one is shown (a dashed line, never a bar).
- Show the numbers that miss their targets, and the ones where another tool wins.
- Comparisons are only fair with the same input, machine and features (source maps, target,
  minify, rule set). If the other tool wasn't run on the same input or did different work, don't
  draw it as a bar next to ours; put it in a table and say why.
- When a newer measurement replaces an old one, update every place it appears:
  `data/bench.json`, `data/conformance.json` and the pages in `content/` (`grep` for the old
  value). Components never hold numbers: the home and tool pages read bench.json through
  `lib/bench.ts`.
- Correctness numbers (the home page's "Correctness" table, each tool page's correctness rows,
  the status page's conformance table) all come from `data/conformance.json`, never typed into
  bench.json: a row with `"home": "<label>"` is the home page row of that name. Each row quotes
  the crate `AGENTS.md` line or README it came from, so the build fails when that line moves on
  without the site. When you re-measure, update the source line first, then the row's `result`,
  `quote` and `commit`.
- `lib/tools.ts`: each tool's copy (what works, what doesn't yet, a `--help` excerpt, a switching
  diff), taken from `content/tools/`, plus which bench.json sections and correctness rows its
  page draws. Keep it in step with those pages; `scripts/check.mjs` checks what it can.

## Docs pages (`content/*.md`, `content/tools/*.md`)

Front matter: `title`, `description` (at most 170 characters), `section` (`Start`, `Tools` or
`Reference`), `order` within the section, optional `nav` (sidebar label), `badges`
(`["warn:Pre-alpha", "ok:…", "next:…"]`) and `generate: status` (the status page's
`<!-- generated:conformance -->` and `<!-- generated:ecosystem -->` markers are replaced from
`data/conformance.json` and `../ecosystem/STATUS.md`). The page body has no H1; `title` is the H1.

- Only facts from the repo: crate `AGENTS.md` files, design records, `--help` output of a built
  binary, measured runs. Never document a flag the binary doesn't print in `--help`.
- Links: relative `.md` links to other pages (`tools/ts-r-dev.md`, `errors/TS2322.md`), so they
  work on GitHub; root links for pages that aren't Markdown (`/benchmarks/#hmr`, `/errors/`,
  `/llms.txt`); relative links to other repo files (`../../crates/ts_r_lsp/AGENTS.md`), which
  become GitHub links.

## Error-code pages (`content/errors/<CODE>.md`)

Front matter: `code` (matches the file name), `summary` (at most 170 characters). TS codes must
be in `data/diagnostics.json`; their message comes from there. Sound-mode codes (TSR) also need
`message`, `planned: true`, `rule`, `rule_title` and `level` from design record 0007.
Sections: `## What it means`, `## Example`, `## How to fix`.

- Examples are ` ```ts example ` blocks (`file=name.ts` for more than one file; `.js` files turn
  on `allowJs`), with an optional ` ```json tsconfig ` block whose `compilerOptions` are merged
  over the default (`strict`, `target: es2022`, `module: esnext`, `moduleResolution: bundler`,
  `noEmit`). An example is shown only when `data/error-examples.json` has a run for its exact
  text in which ts-r reported the code; otherwise the build fails until you run
  `node scripts/verify-examples.mjs CODE`. If ts-r doesn't report the code for any example,
  remove the Example section and say why in the page.
- The verifier needs release builds (`cargo build --release -j 4 -p ts_r_check`, plus
  `-p ts_r_parser --example dump` for TS8xxx codes, which ts-r-check doesn't print yet) and the
  lib files (`cargo xtask upstream sync --minimal` or `TS_R_LIB_DIR`). It runs every example
  under `scripts/limit.sh`.
- TSR examples are ` ```ts spec ` blocks copied from `docs/design/0007-sound-mode/cases/`. The
  site build doesn't run them (the record's case harness in `ts_r_sound` does) and the page
  says so. A retired code (TSR1203) has no page.
- Which codes have pages: see `data/error-selection.json` (at most 300).

## Benchmarks (`data/bench.json`)

- A section has `charts` and/or a `table` (with `tableTitle`), optional `nav` (the contents
  label on `/benchmarks/`), `status` (`{ok, text}` badge) and `notes`. Every section with charts
  needs a `method` with `measured`, `machine` (a key of `machines`), `software`, `input`,
  `command`, `commit` and `sources`; the check fails without them. Source links point at the
  file as of `commit`, so the path must exist in that commit and the commit must be pushed.
- Chart rows have `label` (at most 34 characters, so it fits on a phone), `value`, `kind`
  (`ours`, `other` or `floor`), and optional `display` (for ranges like `4.8–5.0 ms`; plot the
  upper end and say so in the method), `note` and `extra` (values for the chart's `columns`).
  Charts have an `id` (the anchor `#<section>-<chart>`) and take `better: "higher"`, `max`,
  `target` (a dashed line, never a bar) and `verdict` (`{ok, text}`).
- `/benchmarks/` draws every section in full (anchors `#<section id>`, table views, sources);
  the home and tool pages draw the sections `lib/bench.ts` and `lib/tools.ts` name. Bump
  `updated`. Move items out of `notMeasured` only when they have a real measurement.

## Design

- Type: Instrument Sans (text and display; its width axis condenses the big ratio numbers) and
  JetBrains Mono (tool names, code), both from `next/font` and preloaded so the lede doesn't
  shift.
- Colour: ours `#d95926`, theirs `#3987e5`, ink `#0d0f13`. Bars always carry the tool name and
  value as text, so colour is never the only signal.
- Motion: one idea, the race (each bar fills in time proportional to its measured value, `--d`).
  The hero races on load; every other chart (`.reveal`) races once when it scrolls into view
  (`components/reveal.tsx`, an IntersectionObserver). An inline script in the layout adds `io` to
  `<html>` before first paint; only then are charts held at zero, so without JavaScript or under
  `prefers-reduced-motion` they are drawn complete. `<details>` open with a height transition
  where the browser supports `::details-content`. The engine diagram's tool picker is radio
  inputs and `:has()`, no JavaScript.
- Docs, error, search and benchmark pages are the same site: the same header, footer, tokens,
  badges, `.term` code blocks, `.bench-section` charts and pager as the tool pages, with a
  reading layout (`app/docs.css`): docs nav and search on the left, the article at most 46rem,
  "On this page" on the right on wide screens.
- Benchmark tiles are `align-items: start`: opening one tile's "How we measured" never moves the
  others.
- Money: the home page mentions sponsorship once, a quiet line at the end, plus "Sponsor" in the
  header and footer. No donate buttons or tiers on the home, tool or docs pages; tiers live on
  /donate/.
- No all-caps eyebrow labels and no gradient text in headings; one `h1` per page.
- Pages work without JavaScript: search lists every page when the script hasn't run.

## SEO

Metadata API in `app/layout.tsx` and each page (`lib/meta.ts` for the docs pages: canonical,
Open Graph, Twitter card, the Markdown alternate), JSON-LD (Organization, WebSite,
SoftwareApplication, FAQPage, BreadcrumbList, TechArticle on docs and error pages),
`app/sitemap.ts`, `app/robots.ts`, `app/manifest.ts`. `/search/` is noindex. The share image is
`/og.png` (`app/og.png/route.tsx`, rendered at build time with Inter from `@fontsource/inter`);
it has a file extension so static hosts serve it as `image/png`. Fonts come from `next/font` and
are served from ts-r.dev.

## Analytics

PostHog, in both modes, only when `NEXT_PUBLIC_POSTHOG_KEY` is set at build time; local builds
load nothing. `.github/workflows/pages.yml` sets it from the `POSTHOG_KEY` secret or variable,
`NEXT_PUBLIC_POSTHOG_HOST` from the `POSTHOG_HOST` variable (default `https://us.i.posthog.com`),
and `NEXT_PUBLIC_POSTHOG_ENV` (`production` on main, `preview` on pull requests).

- Every page: `instrumentation-client.ts`. `posthog-js` is imported lazily, after `load` and on
  idle: started earlier, it cost the home page's mobile performance score 13 points (LCP 2.6 s
  to 4.6 s). With a test key, mobile: home 97, /docs/ 100, CLS 0.
- Pageviews (including client-side navigation), pageleave and autocaptured clicks come from
  `defaults`. Anonymous visitors get no person profile (`person_profiles: 'identified_only'`).
- Every event carries `site` (`docs` on the docs, error, benchmarks and search pages, set per
  event in `before_send`; `landing` elsewhere) and `site_env` (a super property).
- Pages must work without JavaScript and without PostHog: never make content or navigation
  depend on it. It is the only third-party request the site makes.

## Checks before committing

1. `npm run build` passes in both modes (`SITE_MODE=coming-soon` and `SITE_MODE=launched`): it
   type-checks and runs `scripts/check.mjs`.
2. Lighthouse (`npx -y lighthouse@12`, under `scripts/limit.sh` when in the toolchain repo)
   against `npx serve out` (compressed, like Pages), mobile and desktop: all scores ≥95, CLS ~0.
   Last run (15 Oct 2026, tool pages and charts): home and /tools/ts-r-lsp/, mobile 97 and 98 /
   100 / 100 / 100, CLS 0. The docs, error and benchmark pages haven't been measured since they
   moved into this app.
3. Screenshots at 1400 px and 390 px wide of each page type (home, tool, docs, error, errors
   index, benchmarks, search): no horizontal overflow.
