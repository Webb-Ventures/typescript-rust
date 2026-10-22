// String literal import and export names need `module` other than es2015/es2020 (TS18057).
// @filename: a.ts
const v = 1;
export { v as "<X>" };
export * as "<Z>" from "./b.js";
// @filename: b.ts
import { "<X>" as x } from "./a.js";
export { "<X>" as "<Y>" } from "./a.js";
export const y = x;
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "ES2020", "moduleResolution": "bundler", "noEmit": true}, "files": ["a.ts", "b.ts"]}
