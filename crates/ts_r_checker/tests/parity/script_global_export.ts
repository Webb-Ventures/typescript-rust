// @filename: a.ts
// `export {}` in a `declare global` of a script: `globalThis` is not a local declaration.
declare global {
    var inner: number;
    export { globalThis as global, inner };
}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "noEmit": true }, "files": ["a.ts"] }
