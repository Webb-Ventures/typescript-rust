// An import alias that resolves back to itself is circular (TS2303).
// @filename: a.ts
import type { B } from "./b";
export type { B as A };
// @filename: b.ts
import type { A } from "./a";
export type { A as B };
// @filename: self.ts
import self = require("./self");
export = self;
// @filename: main.ts
namespace M {
    import X = Y;
    import Y = X;
}
import type { A } from "./a";
let a: A;
export {};
