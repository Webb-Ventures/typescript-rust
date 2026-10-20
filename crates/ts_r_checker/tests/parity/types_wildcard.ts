// @filename: tsconfig.json
{ "compilerOptions": { "target": "ES2022", "strict": true, "noEmit": true, "types": ["*"] }, "files": ["a.ts"] }
// @filename: node_modules/@types/a/index.d.ts
declare const fromA: string;
// @filename: node_modules/@types/.b/index.d.ts
declare const fromB: string;
// @filename: a.ts
// `types: ["*"]` includes every package directory of the type roots (not `.`-named ones).
const x: number = fromA;
fromB;
