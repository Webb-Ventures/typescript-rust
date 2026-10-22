// `import.meta` (misspelled too) with `module: commonjs` is TS1343.
// @filename: a.ts
export const m = import.meta;
export const u = import.metal;
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "CommonJS", "noEmit": true}, "files": ["a.ts"]}
