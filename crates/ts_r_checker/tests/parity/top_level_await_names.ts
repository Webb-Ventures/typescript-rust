// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "esnext", "noEmit": true}, "files": ["a.ts", "b.d.ts", "c.ts", "d.mts", "e.ts", "f.ts", "g.ts"]}
// @filename: a.ts
export {};
declare var await: number;
{
    let await = 1;
}
namespace N {
    var await = 2;
}
function f() {
    var await = 3;
}
let [x, { y: await2, await: [await3] }] = [1, { y: 2, await: [3] }];
for (const { await } of [{ await: 1 }]) {}
try {} catch (await) {}
// @filename: b.d.ts
export {};
declare var await: number;
// @filename: c.ts
var await = 1;
// @filename: d.mts
var await = 1;
// @filename: e.ts
import.meta;
export class await {}
// @filename: f.ts
import * as await from "./e";
// @filename: g.ts
import { await as aw } from "./e";
export function await() { return aw; }
