// @filename: a.ts
// Namespace members reached through a type-only import or export, or only through
// `export type *`, are not value properties.
export class A { a!: string }
export interface I {}
export const v = 1;
// @filename: b.ts
export type { A } from "./a";
export type * from "./a";
// @filename: c.ts
import * as b from "./b";
const x = b.A;
const y = b.v;
let t: b.A = { a: "" };
// @filename: d.ts
import type { A as Z } from "./missing";
const Z = 0;
export { Z };
// @filename: e.ts
import * as d from "./d";
let u: typeof d = { Z: 0 };
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "esnext", "noEmit": true }, "files": ["a.ts", "b.ts", "c.ts", "d.ts", "e.ts"] }
