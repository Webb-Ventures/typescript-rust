const a = 1n;
type T = 2n | -3n;
declare const d: 4n;
declare namespace N { const e = 5n; }
let foo = BigInt(2);
foo **= BigInt(2);
const p = BigInt(1) ** BigInt(1);
const q = 2 ** 3;
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2015", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
