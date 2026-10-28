// @filename: tsconfig.json
{ "compilerOptions": { "target": "es2015", "libReplacement": true, "noEmit": true, "strict": true }, "files": ["index.ts"] }
// @filename: node_modules/@typescript/lib-dom/index.d.ts
interface ABC { abc: string }
// @filename: node_modules/@typescript/lib-dom/iterable.d.ts
interface DOMIterable { abc: string }
// @filename: index.ts
/// <reference lib="dom" />
/// <reference lib="dom.iterable" />
// Under libReplacement, a lib whose @typescript/lib-* package resolves is replaced by it.
const a: ABC = { abc: "Hello" };
const b: DOMIterable = { abc: "Hello" };
window.localStorage;
export { a, b };
