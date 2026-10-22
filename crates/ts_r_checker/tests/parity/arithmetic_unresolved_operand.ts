declare const b: boolean;
const r1 = Missing ** [];
const r2 = true * Missing;
let k = 10;
kj **= `2${k}4`;
const r3 = Missing - 1n;
const r4: number = Missing2 * "x";
const r5 = {} * Missing3;
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
