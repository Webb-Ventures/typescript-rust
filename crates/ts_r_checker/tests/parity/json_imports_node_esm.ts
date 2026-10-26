// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"module":"nodenext","resolveJsonModule":true,"noEmit":true},"files":["a.mts","b.cts"]}
// @filename: data.json
{"x":1}
// @filename: a.mts
import d from './data.json';
import { x } from './data.json' with { type: "json" };
export const n = d.x + x;
// @filename: b.cts
import { x } from './data.json';
import * as ns from './data.json';
export const m = x + ns.x;
ns.default;
