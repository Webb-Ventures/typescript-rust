// date-fns `scripts/release/buildChangelog.ts`: `// @ts-nocheck` among the comments before
// the first token turns off semantic checks for the file (`///` and any case too).
/* leading block */
// @ts-nocheck - It's tripping with the new TS/TS config
import { x } from "nope";
const a: number = "";
// @filename: b.ts
const b: number = "";
// @ts-nocheck
export {};
// @filename: c.ts
//@TS-NOCHECK
const c: number = "";
export {};
// @filename: d.ts
/// @ts-nocheck
const d: number = "";
export {};
// @filename: e.ts
/* @ts-nocheck */
const e: number = "";
// @ts-nocheckx
export {};
