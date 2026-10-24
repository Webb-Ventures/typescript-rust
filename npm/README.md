# npm packages (maintainers)

What ships to npm and how it is built. User-facing docs are each package's README
(`ts-r/README.md` for `ts-r`, `typescript/README.md` for `@ts-r.dev/typescript`).

## Packages

Every package also ships ts-r's `LICENSE` (MIT).

| Package | Contains | Depends on |
|---|---|---|
| `@ts-r.dev/<tool>-<platform>` (7 tools x 8 platforms) | `bin/<binary>`; check and lsp also `lib/lib.*.d.ts` + TypeScript's license | nothing; `os`/`cpu`/`libc` fields |
| `@ts-r.dev/run`, `dev`, `build`, `check`, `lint`, `lsp` | the shim as `bin/ts-r-<tool>.js` | its 8 platform packages (optional) |
| `ts-r` | the shim as `bin/ts-r.js` (the launcher) | `@ts-r.dev/cli-<platform>` (optional) and the six tool packages |
| `@ts-r.dev/typescript` | `bin/tsc`, `lib/` (routing, parity check, API re-exports) | `@ts-r.dev/check`; peer `typescript@^7` |

Tools: `cli` (the `ts-r` launcher), `run`, `dev`, `build`, `check`, `lint`, `lsp`. Platforms:
`darwin-arm64`, `darwin-x64`, `linux-arm64-gnu`, `linux-arm64-musl`, `linux-x64-gnu`,
`linux-x64-musl`, `win32-arm64-msvc`, `win32-x64-msvc`. Installing one tool package downloads
that tool's binary for one platform and nothing else (P8: no tool needs another).

All packages share one version (the workspace version), except `@ts-r.dev/typescript`, whose version
is TypeScript's `major.minor` with a patch encoding the ts-r version (ts-r 0.3.1 ->
`7.0.3001`), so `"typescript": "npm:@ts-r.dev/typescript@^7"` resolves.

## Layout

| Path | What |
|---|---|
| `ts-r/bin/ts-r.js` | The one shim, used by `ts-r` and copied into every tool package. It reads its package's `"ts-r"` key (`binary`, `platformPrefix`, `tools`, `version`) |
| `ts-r/package.json`, `ts-r/README.md` | Template and README of `@ts-r.dev/cli`, the `ts-r` launcher package |
| `ts-r/problem-matcher.json` | The GitHub Actions problem matcher for `TS` and `TSR` errors, shipped in `ts-r` and `@ts-r.dev/check` |
| `typescript/` | `@ts-r.dev/typescript` (see its README for the exact behaviour) |
| `scripts/build-packages.mjs` | Builds every package from cargo output into `dist/` (or `--out`), with `packages.json` listing them in publish order |
| `scripts/gen-typescript-api.mjs` | Regenerates `typescript/lib/unstable/` from the pinned TypeScript (after a bump) |
| `tests/fixtures/` | Projects for the smoke tests: `pass`, `errors` (ts-r-check matches tsc 7), `mismatch` (it doesn't) |
| `tests/*.test.cjs` | Unit tests for the shims: `node --test npm/tests/*.test.cjs` |

Platform tables must agree in `scripts/build-packages.mjs` (`TARGETS`), `ts-r/bin/ts-r.js` and
`typescript/lib/real.cjs` (`PLATFORMS`); the build script fails otherwise.

## Building and testing locally

```sh
cargo xtask upstream sync --minimal          # TypeScript's lib files for check/lsp packages
cargo xtask npm-pack                          # build, then npm pack --dry-run: contents, sizes
cargo xtask npm-pack --smoke                  # also install each package from tarballs and run it
cargo xtask npm-pack --bin-dir target/pgo/optimized/aarch64-apple-darwin/release --smoke
```

`--smoke` installs every tool package alone in its own temp project (`npm install --offline
--ignore-scripts`), checks nothing else came with it, runs `--version` and a smoke command
(`ts-r-run hello.ts`, a bundle, a check with the packaged lib files, a lint, a dev server
request, `ts-r-lsp --help`, `ts-r run` through the launcher), and runs the `@ts-r.dev/typescript`
test: `npx tsc --noEmit` through the drop-in on each fixture must give the real tsc's exit code
and stdout, with the parity check passing on `pass` and `errors` and failing on `mismatch`.

Without building packages: `TS_R_BIN_DIR=target/release node npm/ts-r/bin/ts-r.js --version`.

## Publishing

Done by the public repo's `.github/workflows/release.yml`, run by hand on `main` with `dry_run`
off after a release pull request (a `release/vX.Y.Z[-<label>]` branch into `main`) has set the
version; it tags `main` `vVERSION`. Nothing publishes automatically, and a run with `dry_run` on
(the default) only rehearses. It publishes in two parallel phases
(`.github/scripts/npm-publish.sh` there): every platform package at once, then every front package
(the tool packages, `@ts-r.dev/cli` and `@ts-r.dev/typescript`) at once, each with
`npm publish --provenance --access public --tag <latest|next>`. A failed platform phase stops the
run before any front goes out. Versions already on npm are skipped, so a re-run publishes only
what is missing.
`repository.url` in every package must name the
GitHub repository that runs the workflow, or npm rejects the provenance statement
(`--repository`, default `$GITHUB_REPOSITORY`).

The workflow publishes with npm trusted publishing (OIDC): no long-lived token. The npm job
needs `id-token: write`, npm 11.5.1 or newer (installed explicitly) and a GitHub-hosted runner
(`ubuntu-latest`; npm doesn't accept self-hosted runners). npm
only lets you add a trusted publisher to a package that already exists, so a package's first
publish needs a token: when secret `NPM_TOKEN` is set the run uses it, otherwise OIDC. The
publish step logs which one it used. Provenance is automatic with trusted publishing.

### One-time setup per package

1. Publish the package once: set secret `NPM_TOKEN` to a granular access token (packages and
   scopes: read and write, all packages; bypass 2FA; short expiry) and run the workflow, or
   publish locally with 2FA.
2. On npmjs.com, the package's Settings, Trusted publishing: add a trusted publisher with
   GitHub Actions, organization or user `Webb-Ventures`, repository `typescript-rust`, workflow
   `release.yml`, environment `release`.
3. Same page, Publishing access: "Require two-factor authentication and disallow tokens".

When every package has a trusted publisher, delete secret `NPM_TOKEN` and revoke the token. A
package added later (a new tool or platform) goes through the same three steps.

All 64 packages belong to the `ts-r.dev` npm org (an unscoped `ts-r` is refused: npm treats it as too
close to the existing `tsr`):

- `@ts-r.dev/cli` (the `ts-r` launcher) and `@ts-r.dev/typescript`
- tool packages: `@ts-r.dev/run`, `@ts-r.dev/dev`, `@ts-r.dev/build`, `@ts-r.dev/check`,
  `@ts-r.dev/lint`, `@ts-r.dev/lsp`
- platform packages, `@ts-r.dev/<tool>-<platform>` for the 7 tools `cli`, `run`, `dev`,
  `build`, `check`, `lint`, `lsp` and the 8 platforms:

| Tool | Packages |
|---|---|
| `cli` | `@ts-r.dev/cli-darwin-arm64`, `@ts-r.dev/cli-darwin-x64`, `@ts-r.dev/cli-linux-arm64-gnu`, `@ts-r.dev/cli-linux-arm64-musl`, `@ts-r.dev/cli-linux-x64-gnu`, `@ts-r.dev/cli-linux-x64-musl`, `@ts-r.dev/cli-win32-arm64-msvc`, `@ts-r.dev/cli-win32-x64-msvc` |
| `run` | `@ts-r.dev/run-darwin-arm64`, `@ts-r.dev/run-darwin-x64`, `@ts-r.dev/run-linux-arm64-gnu`, `@ts-r.dev/run-linux-arm64-musl`, `@ts-r.dev/run-linux-x64-gnu`, `@ts-r.dev/run-linux-x64-musl`, `@ts-r.dev/run-win32-arm64-msvc`, `@ts-r.dev/run-win32-x64-msvc` |
| `dev` | `@ts-r.dev/dev-darwin-arm64`, `@ts-r.dev/dev-darwin-x64`, `@ts-r.dev/dev-linux-arm64-gnu`, `@ts-r.dev/dev-linux-arm64-musl`, `@ts-r.dev/dev-linux-x64-gnu`, `@ts-r.dev/dev-linux-x64-musl`, `@ts-r.dev/dev-win32-arm64-msvc`, `@ts-r.dev/dev-win32-x64-msvc` |
| `build` | `@ts-r.dev/build-darwin-arm64`, `@ts-r.dev/build-darwin-x64`, `@ts-r.dev/build-linux-arm64-gnu`, `@ts-r.dev/build-linux-arm64-musl`, `@ts-r.dev/build-linux-x64-gnu`, `@ts-r.dev/build-linux-x64-musl`, `@ts-r.dev/build-win32-arm64-msvc`, `@ts-r.dev/build-win32-x64-msvc` |
| `check` | `@ts-r.dev/check-darwin-arm64`, `@ts-r.dev/check-darwin-x64`, `@ts-r.dev/check-linux-arm64-gnu`, `@ts-r.dev/check-linux-arm64-musl`, `@ts-r.dev/check-linux-x64-gnu`, `@ts-r.dev/check-linux-x64-musl`, `@ts-r.dev/check-win32-arm64-msvc`, `@ts-r.dev/check-win32-x64-msvc` |
| `lint` | `@ts-r.dev/lint-darwin-arm64`, `@ts-r.dev/lint-darwin-x64`, `@ts-r.dev/lint-linux-arm64-gnu`, `@ts-r.dev/lint-linux-arm64-musl`, `@ts-r.dev/lint-linux-x64-gnu`, `@ts-r.dev/lint-linux-x64-musl`, `@ts-r.dev/lint-win32-arm64-msvc`, `@ts-r.dev/lint-win32-x64-msvc` |
| `lsp` | `@ts-r.dev/lsp-darwin-arm64`, `@ts-r.dev/lsp-darwin-x64`, `@ts-r.dev/lsp-linux-arm64-gnu`, `@ts-r.dev/lsp-linux-arm64-musl`, `@ts-r.dev/lsp-linux-x64-gnu`, `@ts-r.dev/lsp-linux-x64-musl`, `@ts-r.dev/lsp-win32-arm64-msvc`, `@ts-r.dev/lsp-win32-x64-msvc` |
