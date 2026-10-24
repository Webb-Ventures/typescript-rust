// TS1009: a trailing comma in an import call is a grammar error below the modules that take
// import attributes (dynamicImportTrailingComma, importAttributes1).
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "lib": ["ES2020"], "module": "commonjs", "noEmit": true }, "files": ["a.ts", "b.ts"] }
// @filename: b.ts
export const v = 1;
// @filename: a.ts
export const p = import('./b',);
export const q = import('./b', {},);
export const r = import('./b' /* c */ ,
    // trailing
);
export const s = import('./b', {});
