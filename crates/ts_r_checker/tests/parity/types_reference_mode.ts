// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "node16", "noEmit": true, "types": []}, "files": ["index.ts", "esm/forced.ts"]}
// @filename: node_modules/pkg/package.json
{"name": "pkg", "version": "0.0.1", "exports": {"import": "./import.js", "require": "./require.js"}}
// @filename: node_modules/pkg/import.d.ts
export {};
declare global {
    var foo: number;
}
// @filename: node_modules/pkg/require.d.ts
export {};
declare global {
    var bar: number;
}
// @filename: index.ts
/// <reference types="pkg" />
foo;
bar;
export {};
// @filename: esm/package.json
{"type": "module"}
// @filename: esm/forced.ts
/// <reference types="pkg" resolution-mode="require" />
export {};
