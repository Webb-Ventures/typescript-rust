declare const x: number;
if (x === NaN) {}
if (NaN != x) {}
if ((x) == (NaN)) {}
if (NaN === NaN) {}
if (x + 1 !== NaN) {}
declare const o: { a: { b: number } };
if (o.a.b === NaN) {}
function f(NaN: number) { return x === NaN; }
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
