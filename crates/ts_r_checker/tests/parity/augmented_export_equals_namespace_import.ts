// A package whose `export =` is a namespace import of another file, augmented by its own
// entry (`declare module "pkg" { }`): the augmentation merges into that file's module,
// which keeps its exports; the default import (and `import * as`) reads them.
import pkg from "pkg";
import * as star from "pkg";
pkg.initializeApp();
star.initializeApp();
const n: number = pkg.extra;
pkg.missing();
// @filename: package.json
{ "type": "module" }
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "module": "nodenext", "target": "esnext", "noEmit": true }, "files": ["a.ts"] }
// @filename: node_modules/pkg/package.json
{ "name": "pkg", "main": "lib/index.js", "types": "./lib/index.d.ts", "exports": { ".": "./lib/index.js" } }
// @filename: node_modules/pkg/lib/index.d.ts
import * as ns from './ns';
declare module 'pkg' {
    export const extra: number;
}
export = ns;
// @filename: node_modules/pkg/lib/ns.d.ts
export * from './api';
// @filename: node_modules/pkg/lib/api.d.ts
export declare function initializeApp(): void;
