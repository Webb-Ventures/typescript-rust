// A namespace import of an `export =` value narrows like a constant.
// @filename: b.d.ts
declare const b: number | undefined;
declare namespace b {}
export = b;
// @filename: main.ts
import * as b0 from "./b";
let x: number;
if (b0) x = b0;
x = b0;
