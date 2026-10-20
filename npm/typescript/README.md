# @ts-r.dev/typescript

A drop-in for the `typescript` package. Its `tsc` type-checks with
[ts-r-check](https://www.npmjs.com/package/@ts-r.dev/check) when ts-r-check gives the same result as
your own TypeScript on that project, and runs your TypeScript for everything else. Its module API
(`typescript`, `typescript/unstable/*`) is your TypeScript's.

Your real TypeScript (7.x) stays installed: it is a peer dependency, and the fallback for every
flag, project and API ts-r doesn't cover.

## Install

Either way works; undo it by deleting the line(s).

**Next to TypeScript** (point `typescript.tsdk`, scripts or tools at this package):

```jsonc
// package.json
"devDependencies": {
  "typescript": "^7.0.0",
  "@ts-r.dev/typescript": "^7"
}
```

Run it as `npx -p @ts-r.dev/typescript tsc` or `node_modules/@ts-r.dev/typescript/bin/tsc`, or set
`"typescript.tsdk": "node_modules/@ts-r.dev/typescript/lib"`.

**In place of TypeScript** (every tool that runs `tsc` or imports `typescript` gets this package):

```jsonc
"devDependencies": {
  "typescript": "npm:@ts-r.dev/typescript@^7",
  "typescript-real": "npm:typescript@^7.0.0"
}
```

The `typescript-real` alias is how the drop-in finds your TypeScript when it occupies the
`typescript` name itself.

## What `tsc` does

Every run goes through these steps, in order. The first one that applies decides.

1. **`TS_R_TSC=typescript`**: run your TypeScript's `tsc` with the arguments unchanged.
2. **Flags ts-r-check doesn't support go to your TypeScript.** ts-r-check takes
   `-p`/`--project <file or dir>`, `--noEmit [true|false]` and `--pretty [true|false]` (names
   are case-insensitive, like tsc's). Anything else (file names, `--build`, `--watch`,
   `--declaration`, `--outDir`, `--strict`, `--init`, `--showConfig`, `--listFiles`, ...) runs
   your `tsc` with the arguments unchanged and prints one line on stderr:
   `@ts-r.dev/typescript: ts-r-check doesn't support --watch; running TypeScript 7.0.2's tsc.`
   `--version`, `-v`, `--help`, `-h` and `--all` go to your TypeScript without that line.
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
   - The first run for a project runs ts-r-check and your `tsc` side by side, both with
     `-p <tsconfig> --noEmit --pretty false` (ts-r-check also with `--sound=off`), and compares
     their exit codes and their diagnostics as multisets of whole diagnostics (location, code,
     message and every chained line).
   - **Match**: this run and later runs use ts-r-check. **Mismatch**: this run and later runs
     use your TypeScript for that project, and a stderr note gives the path of a plain-text
     report listing the diagnostics only one side produced.
   - The result is cached in `<ts-r cache>/typescript/parity/<key>.json` (the cache is
     `$TS_R_CACHE_DIR`, else `$XDG_CACHE_HOME/ts-r`, `%LOCALAPPDATA%\ts-r` or `~/.cache/ts-r`).
     The key covers the tsconfig path, the text of the tsconfig and every config it extends, the
     nearest lockfile, your TypeScript's version and the ts-r-check version and binary. Editing
     source files doesn't re-run the check; changing any of those does. Delete the directory to
     force a re-check.
   - On that first run, when the output isn't pretty (`--pretty false`, `"pretty": false`, or
     stdout isn't a terminal), the captured output of the chosen side is printed and its exit
     code returned, so nothing runs twice. With pretty output, the chosen side runs again with
     your arguments.

When ts-r-check runs, it gets `-p <tsconfig> --noEmit --sound=off --pretty <true|false>` (your
`--pretty`, else tsconfig's `"pretty": false`, else whether stdout is a terminal). Its
output format and exit codes are tsc's: 0 without errors, 1 with errors (no emit happens).
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

## Editors

`typescript.tsdk` pointed at `node_modules/@ts-r.dev/typescript/lib` keeps the editor on your
TypeScript (VS Code's TypeScript 7 support finds the native `@typescript/typescript-<platform>`
package from there). For ts-r in the editor, use the ts-r VS Code extension (`ts-r-lsp`), which
applies the same parity rule per project.

## Limits

- `tsc --build`, `--watch`, emitting builds and project references always run your TypeScript.
- `incremental`/`composite` projects checked by ts-r-check don't update `.tsbuildinfo`.
- tsserver plugins, TypeScript 5/6 as a library (typescript-eslint, ts-morph) need TypeScript
  5/6 installed under its own name, the same as with TypeScript 7 itself.
