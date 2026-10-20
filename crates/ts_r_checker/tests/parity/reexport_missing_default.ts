// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "commonjs", "noEmit": true}, "files": ["a.ts", "b.ts", "c.d.ts", "d.ts", "e.ts"]}
// @filename: b.ts
export const b = null;
// @filename: c.d.ts
declare var c: number;
export { c };
// @filename: d.ts
const d = 1;
export = d;
// @filename: e.ts
export default 1;
// @filename: a.ts
export { b } from "./b";
export { default } from "./b";
export { default as fromC } from "./c";
export { default as fromD } from "./d";
export { default as fromE } from "./e";
export { default as againB } from "./b";
