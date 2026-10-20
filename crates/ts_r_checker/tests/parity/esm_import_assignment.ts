// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "esnext", "moduleResolution": "bundler", "outDir": "out"}, "files": ["a.ts", "b.d.ts", "c.d.mts", "d.mts", "e.cts", "sub/h.ts", "g.ts"]}
// @filename: package.json
{"name": "p", "type": "module"}
// @filename: sub/package.json
{"type": "commonjs"}
// @filename: x.cts
export const x = 1;
// @filename: a.ts
import a = require("./x.cjs");
export { a };
// @filename: b.d.ts
import b = require("./x.cjs");
export { b };
// @filename: c.d.mts
import c = require("./x.cjs");
export { c };
// @filename: d.mts
import d = require("./x.cjs");
export { d };
// @filename: e.cts
import e = require("./x.cjs");
export { e };
// @filename: sub/h.ts
import h = require("../x.cjs");
export { h };
// @filename: g.ts
import type t = require("./x.cjs");
declare module "m" {
    import y = require("./x.cjs");
}
namespace N { export const v = 1; }
import v = N.v;
export type { t };
export { v };
