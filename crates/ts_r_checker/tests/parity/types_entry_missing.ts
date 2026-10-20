// @filename: tsconfig.json
{ "compilerOptions": { "target": "ES2022", "strict": true, "noEmit": true, "types": ["nope"] }, "files": ["a.ts"] }
// @filename: a.ts
// A `types` entry that resolves to nothing: TS2688 says why it is in the program.
const x: number = 1;
