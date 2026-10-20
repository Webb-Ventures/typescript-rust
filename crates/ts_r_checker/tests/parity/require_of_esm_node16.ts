// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "node16", "allowJs": true, "checkJs": true, "resolveJsonModule": true, "noEmit": true}, "files": ["esm.mts", "a.ts", "b.tsx", "c.cts", "sub/d.ts", "e.js", "f.ts"]}
// @filename: package.json
{"name": "p"}
// @filename: sub/package.json
{"type": "commonjs"}
// @filename: esm.mts
export const x = 1;
// @filename: a.ts
import { x } from "./esm.mjs";
import type { x as y } from "./esm.mjs";
export { x as z } from "./esm.mjs";
import m = require("./esm.mjs");
export async function f() {
    return (await import("./esm.mjs")).x;
}
x; m;
export type Y = typeof y;
export type T1 = typeof import("./esm.mjs");
export type T2 = typeof import("./esm.mjs", { with: { "resolution-mode": "import" } });
import type { x as w } from "./esm.mjs" with { "resolution-mode": "import" };
export type W = typeof w;
// @filename: b.tsx
import { x } from "./esm.mjs";
x;
// @filename: c.cts
import { x } from "./esm.mjs";
x;
// @filename: sub/d.ts
import { x } from "../esm.mjs";
x;
// @filename: e.js
import { x } from "./esm.mjs";
x;
// @filename: f.ts
import data = require("./package.json");
data;
