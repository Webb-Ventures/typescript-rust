// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "nodenext", "allowJs": true, "checkJs": true, "noEmit": true}, "files": ["a.ts", "b.mts", "c.cts", "d.js", "e.ts"]}
// @filename: package.json
{"name": "p", "type": "commonjs"}
// @filename: a.ts
const x = await 1;
for await (const y of []) {}
for /* c */ await (const z of []) {}
if (x) { await x; }
async function f() { await 1; for await (const w of []) {} }
class C { m = async () => await 1; }
export const u = import.meta.url;
function g() { return import.meta; }
export { x, f, C, g };
// @filename: b.mts
const x = await 1;
for await (const y of []) {}
export const u = import.meta.url;
export { x };
// @filename: c.cts
const x = await 1;
export const u = import.meta.url;
export { x };
// @filename: d.js
const x = await 1;
export const u = import.meta.url;
export { x };
// @filename: e.ts
export const a = await (1);
export const b = await [1];
export const d = await "s";
export const e = await new Date();
