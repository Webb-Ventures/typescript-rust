// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "nodenext", "noEmit": true}, "files": ["a.mts", "b.cts"]}
// @filename: node_modules/pa/package.json
{"name": "pa", "version": "1.0.0"}
// @filename: node_modules/pa/index.d.ts
export const a: number;
// @filename: node_modules/pb/package.json
{"name": "pb", "version": "1.0.0", "main": "lib.js"}
// @filename: node_modules/pb/index.d.ts
export const b: number;
// @filename: node_modules/pc/package.json
{"name": "pc", "version": "1.0.0", "types": "missing.d.ts"}
// @filename: node_modules/pc/index.d.ts
export const c: number;
// @filename: node_modules/pd/index.d.ts
export const d: number;
// @filename: node_modules/pe/package.json
{"name": "pe", "version": "1.0.0"}
// @filename: node_modules/pe/sub/index.d.ts
export const e: number;
// @filename: node_modules/pe/sub2/package.json
{"name": "pe2"}
// @filename: node_modules/pe/sub2/index.d.ts
export const e2: number;
// @filename: a.mts
import { a } from "pa";
import { b } from "pb";
import { c } from "pc";
import { d } from "pd";
import { e } from "pe/sub";
import { e2 } from "pe/sub2";
export { a, b, c, d, e, e2 };
// @filename: b.cts
import { a } from "pa";
import { b } from "pb";
import { c } from "pc";
import { d } from "pd";
export { a, b, c, d };
