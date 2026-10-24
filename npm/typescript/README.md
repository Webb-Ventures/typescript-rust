# @ts-r.dev/typescript

A drop-in for the `typescript` package. Its `tsc` type-checks with
[ts-r-check](https://www.npmjs.com/package/@ts-r.dev/check) when ts-r-check gives the same result as
your own TypeScript on that project, and runs your TypeScript for everything else. Its module API
(`typescript`, `typescript/unstable/*`) is your TypeScript's.

Your real TypeScript (7.x) stays installed: it is a peer dependency (or the `typescript-real`
alias), and the fallback for every flag, project and API ts-r doesn't cover.

## Install

Either way works; undo it by deleting the line(s). While ts-r is in alpha, name an exact
version (below, `7.0.1000-alpha.3`; `npm view @ts-r.dev/typescript dist-tags` lists the
current ones) or the `next` dist-tag, not a range like `^7`: see [Versions](#versions).

**Next to TypeScript** (point `typescript.tsdk`, scripts or tools at this package):

```jsonc
// package.json
"devDependencies": {
  "typescript": "^7.0.0",
  "@ts-r.dev/typescript": "7.0.1000-alpha.3"
}
```

or `npm install --save-dev typescript@7 @ts-r.dev/typescript@next`. Run it as
`npx -p @ts-r.dev/typescript tsc` or `node_modules/@ts-r.dev/typescript/bin/tsc`, or set
`"typescript.tsdk": "node_modules/@ts-r.dev/typescript/lib"`.

**In place of TypeScript** (every tool that runs `tsc` or imports `typescript` gets this package):

```jsonc
"devDependencies": {
  "typescript": "npm:@ts-r.dev/typescript@7.0.1000-alpha.3",
  "typescript-real": "npm:typescript@^7.0.0"
}
```

(`npm:@ts-r.dev/typescript@next` works too.) The `typescript-real` alias is how the drop-in
finds your TypeScript when it occupies the `typescript` name itself. Tools that find
TypeScript by resolving `typescript` from your project, such as `next build`, only use the
drop-in this way (see [Next.js](#nextjs)).

### Versions

The version is TypeScript's `major.minor`, a patch that encodes the ts-r version (ts-r
0.1.0 -> `7.0.1000`, 1.2.3 -> `7.0.1002003`) and ts-r's prerelease label: ts-r
0.1.0-alpha.3 is `7.0.1000-alpha.3`. By semver's rule, which npm follows, a range like `^7` or
`^7.0.0` never matches a prerelease; a range only matches prereleases of the exact
`major.minor.patch` it names itself (`^7.0.1000-alpha.3` matches `7.0.1000-alpha.4`, not
`7.0.2000-alpha.1`). That is deliberate on npm's side (nobody gets an alpha without asking for
one), and the drop-in keeps the label so its alphas are marked and ordered the same way as
ts-r's. So during the alpha, pin an exact version or use `next`; `^7` starts to work with
ts-r's first stable release.

The `typescript` peer dependency is `^7.0.0` plus the drop-in's own version (for
`7.0.1000-alpha.3`: `^7.0.0 || 7.0.1000-alpha.3`). Installed in place of TypeScript, the
`typescript` name is the drop-in itself, so npm checks the peer against the drop-in's version;
without its own version in the range, an alpha fails that check (`ERESOLVE`). Installed next to
TypeScript, the peer is your TypeScript and must match `^7.0.0`.

## What `tsc` does

Every run goes through these steps, in order. The first one that applies decides.

1. **`TS_R_TSC=typescript`**: run your TypeScript's `tsc` with the arguments unchanged.
2. **Flags ts-r-check doesn't support go to your TypeScript.** The drop-in takes
   `-p`/`--project <file or dir>`, `--noEmit [true|false]`, `--pretty [true|false]`,
   `--tsBuildInfoFile <file>`, and these emit options when set to `false`: `--declaration`
   (`-d`), `--declarationMap`, `--emitDeclarationOnly`, `--sourceMap`, `--inlineSourceMap` and
   `--inlineSources` (option names are case-insensitive, like tsc's). Anything else (file
   names, `--build`, `--watch`, `--declaration` on its own or `true`, `--outDir`, `--strict`,
   `--init`, `--listFiles`, ...) runs your `tsc` with the arguments unchanged and prints one line
   on stderr:
   `@ts-r.dev/typescript: ts-r-check doesn't support --watch; running TypeScript 7.0.2's tsc.`
   `--version`, `-v`, `--help`, `-h`, `--all` and `--showConfig` go to your TypeScript without
   that line.
3. **No tsconfig: your TypeScript.** The project is `-p`'s file (or `<dir>/tsconfig.json`), else
   the nearest `tsconfig.json` from the working directory up. If there is none, your `tsc` runs
   and reports it.
4. **Projects that emit go to your TypeScript.** ts-r-check never writes files. Unless
   `--noEmit` is given (or `noEmit: true` is set in the tsconfig or a config it `extends`), your
   `tsc` runs and emits, with a stderr note. A tsconfig (or an `extends` target) that can't be
   read or resolved is treated the same way.
5. **ts-r-check must be installed** (`@ts-r.dev/check`'s binary for your platform); otherwise your
   `tsc` runs, with a note.
6. **`TS_R_TSC=ts-r`**: run ts-r-check without the parity check.
7. **Without your TypeScript there is no parity check**: ts-r-check runs, with a note.
8. **The parity check decides, per project** (the plan's per-project parity rule, the same one
   `ts-r-lsp` uses):
   - The first run for a project runs ts-r-check and your `tsc` side by side, both with your
     `-p` (if you gave one) and your other accepted options (`--tsBuildInfoFile`, the emit
     options set to `false`) as you gave them, then `--noEmit --pretty false` (ts-r-check also
     with `--sound=off`), and compares their exit codes and their diagnostics as multisets of
     whole diagnostics (location, code, message and every chained line).
   - **Match**: this run and later runs use ts-r-check. **Mismatch**: this run and later runs
     use your TypeScript for that project, and a stderr note gives the path of a plain-text
     report listing the diagnostics only one side produced.
   - The result is cached in `<ts-r cache>/typescript/parity/<key>.json` (the cache is
     `$TS_R_CACHE_DIR`, else `$XDG_CACHE_HOME/ts-r`, `%LOCALAPPDATA%\ts-r` or `~/.cache/ts-r`).
     The key covers the tsconfig path, the text of the tsconfig and every config it extends, the
     nearest lockfile, your TypeScript's version, the ts-r-check version and binary, and the
     options passed to both checkers (so `tsc` and `tsc --declarationMap false` are checked
     separately). Editing source files doesn't re-run the check; changing any of those does.
     Delete the directory to force a re-check.
   - On that first run, when the output isn't pretty (see below), the captured output of the
     chosen side is printed and its exit code returned, so nothing runs twice. With pretty
     output, the chosen side runs again with your arguments.

When ts-r-check runs, it gets your `-p` only if you gave one (without it, ts-r-check finds the
nearest `tsconfig.json` the way tsc does, and diagnostic paths stay relative to the working
directory like tsc's), your other accepted options as given, then
`--noEmit --sound=off --pretty <true|false>`. Pretty output follows tsc 7: your `--pretty`,
else off when `NO_COLOR` is set (non-empty), else on when `FORCE_COLOR` is set (non-empty),
else whether stdout is a terminal; like tsc 7, `pretty` in tsconfig is ignored. Its output
format and exit codes are tsc's: 0 without errors, 1 with errors (no emit happens).
Diagnostics come sorted by file and position; tsc prints them in program order, so the order can
differ even when the parity check passed (it compares multisets).

Environment: `TS_R_TSC=auto|ts-r|typescript` (default `auto`), `TS_R_TYPESCRIPT=<dir>` (your
TypeScript's package directory), `TS_R_TSC_QUIET=1` (no notes), `TS_R_TSC_VERBOSE=1` (also
print the parity result of every run), `TS_R_BIN_DIR=<dir>` (use the ts-r-check in `<dir>`).

## Finding your TypeScript

In order: `$TS_R_TYPESCRIPT`; then `typescript`, if it is the real package and not this one;
then `typescript-real`. Each name is resolved from the working directory first, then from this
package. Your TypeScript's `tsc` runs as `node <typescript>/bin/tsc <args>` with inherited stdio,
so its output and exit code are exactly its own.

## Module API

- `require("typescript")` / `import "typescript"`: `{ version, versionMajorMinor }` of your
  TypeScript (TypeScript 7's main entry exports only these), or of the TypeScript this package
  was built against (7.0.2) when yours isn't found.
- `typescript/unstable/sync`, `unstable/async`, `unstable/fs`, `unstable/proto`, `unstable/ast`
  and `unstable/ast/{is,factory,utils,scanner,visitor,clone}`: each is an ES module that imports
  the same subpath of your TypeScript (resolved through its own `exports`) and re-exports its
  named exports. The list of names is generated from TypeScript 7.0.2
  (`npm/scripts/gen-typescript-api.mjs`); a name your version adds is reachable through your
  TypeScript directly. Without your TypeScript, importing these throws an error saying how to
  install it.
- Types: `typescript/unstable/*` re-export `typescript/unstable/*` types, which resolve to your
  TypeScript when it is installed under its own name ("Next to TypeScript"). Installed in place
  of `typescript`, type-check code that imports TypeScript's API with a `paths` entry pointing
  `typescript/*` at `typescript-real/*`.
- Nothing in the API is implemented by ts-r yet, so the "everything goes to your TypeScript"
  part of the parity rule holds for the API at all times.

## Next.js

`next build` type-checks by running the `tsc` of the `typescript` package it resolves from your
project, so it uses the drop-in when the drop-in is installed in place of TypeScript. Next.js 16
runs `tsc --showConfig --project <tsconfig> --pretty false` (this goes to your TypeScript), then
`tsc --project <tsconfig> --noEmit --declarationMap false --emitDeclarationOnly false`, plus
`--tsBuildInfoFile .next/cache/.tsbuildinfo` for an `incremental` project (Next.js's default
tsconfig is). The drop-in takes that second command, so the check runs on ts-r-check once the
parity check passes, and on your TypeScript otherwise.

## Editors

`typescript.tsdk` pointed at `node_modules/@ts-r.dev/typescript/lib` keeps the editor on your
TypeScript (VS Code's TypeScript 7 support finds the native `@typescript/typescript-<platform>`
package from there). For ts-r in the editor, use the ts-r VS Code extension (`ts-r-lsp`), which
applies the same parity rule per project.

## Limits

- `tsc --build`, `--watch`, emitting builds and project references always run your TypeScript.
- For `incremental`/`composite` projects ts-r-check writes a `.tsbuildinfo` in its own format at
  the path tsc uses (`--tsBuildInfoFile`/`tsBuildInfoFile`, else tsc's default). tsc reads
  that file as out of date and checks the whole project once, then writes its own; ts-r-check
  does the same with tsc's, so switching between the two costs one full check each way.
- tsserver plugins, TypeScript 5/6 as a library (typescript-eslint, ts-morph) need TypeScript
  5/6 installed under its own name, the same as with TypeScript 7 itself.
