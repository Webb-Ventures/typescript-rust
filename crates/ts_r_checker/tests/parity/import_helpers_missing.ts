// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "commonjs", "importHelpers": true, "noEmit": true}, "files": ["types.d.ts", "a.ts", "b.ts", "c.ts", "e.ts", "f.ts", "g.d.ts", "h.ts", "i.ts", "j.mts"]}
// @filename: types.d.ts
declare module "fs";
declare module "tslib" {
    export {};
}
// @filename: a.ts
import unused from "fs";
import * as unusedNs from "fs";
export {};
// @filename: b.ts
import type tdef from "fs";
import type * as tns from "fs";
export type { default as T } from "fs";
export type * as Q from "fs";
export type * from "fs";
// @filename: c.ts
import d2, * as ns2 from "fs";
import { default as d4 } from "fs";
d2; ns2; d4;
// @filename: e.ts
import { x, default as d5 } from "fs";
x; d5;
// @filename: f.ts
import d6, { y } from "fs";
d6; y;
// @filename: g.d.ts
import d from "fs";
import * as ns from "fs";
export * from "fs";
// @filename: h.ts
export * from "fs";
export * as nsb from "fs";
export { default } from "fs";
export { default as dd, z } from "fs";
// @filename: i.ts
export type { default as T } from "fs";
// @filename: j.mts
import d from "fs";
import * as ns from "fs";
d; ns;
export * from "fs";
