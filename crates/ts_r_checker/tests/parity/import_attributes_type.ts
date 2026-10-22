// Import attributes: values must be string literals (TS2858) and the attributes object
// must be assignable to `ImportAttributes`; `import()` options to `ImportCallOptions`.
import { x } from "./b" with { type: "json", other: `t` };
import { y } from "./b" with { type: "json" };
const m = import("./b", { with: { type: "json" }, "extra-key": 1 });
const n = import("./b", { with: { kind: 1 } });
type W = string[{ a: "x" }];
export const z = [x, y, m, n] as const;
// @filename: b.ts
export const x = 1;
export const y = 2;
