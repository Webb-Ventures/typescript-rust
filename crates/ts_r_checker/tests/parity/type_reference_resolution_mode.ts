// @filename: tsconfig.json
{ "compilerOptions": { "module": "node16", "target": "ES2022", "strict": true, "noEmit": true, "types": [] }, "files": ["cjs.ts", "esm.mts"] }
// @filename: node_modules/pkg/package.json
{ "name": "pkg", "version": "0.0.1", "exports": { "import": "./import.d.ts", "require": "./require.d.ts" } }
// @filename: node_modules/pkg/import.d.ts
export {};
declare global { var fromImport: number; }
// @filename: node_modules/pkg/require.d.ts
export {};
declare global { var fromRequire: number; }
// @filename: node_modules/other/package.json
{ "name": "other", "version": "0.0.1", "exports": { "import": "./import.d.ts", "require": "./require.d.ts" } }
// @filename: node_modules/other/import.d.ts
export {};
declare global { var otherImport: number; }
// @filename: node_modules/other/require.d.ts
export {};
declare global { var otherRequire: number; }
// @filename: cjs.ts
/// <reference types="pkg" />
// A types reference resolves in its file's mode (CommonJS here: the `require` condition)
// unless `resolution-mode` says otherwise.
fromRequire;
fromImport;
export {};
// @filename: esm.mts
/// <reference types="other" resolution-mode="require" />
otherRequire;
otherImport;
export {};
