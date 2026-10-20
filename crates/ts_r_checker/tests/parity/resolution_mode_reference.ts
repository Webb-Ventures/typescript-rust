// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "node16", "noEmit": true}, "files": ["a.ts", "b.ts", "c.ts"]}
// @filename: node_modules/pkg/package.json
{"name": "pkg", "version": "1.0.0", "exports": {"import": "./i.js", "require": "./r.js"}}
// @filename: node_modules/pkg/i.d.ts
export {};
declare global { var i: number; }
// @filename: node_modules/pkg/r.d.ts
export {};
declare global { var r: number; }
// @filename: a.ts
/// <reference types="pkg" resolution-mode="esm" />
/// <reference types="pkg" resolution-mode='import' />
export {};
// @filename: b.ts
/// Example: <reference types="pkg" resolution-mode="bogus" />
export {};
// @filename: c.ts
/// <reference path="./types.ts" resolution-mode="bogus" />
export {};
// @filename: types.ts
export {};
