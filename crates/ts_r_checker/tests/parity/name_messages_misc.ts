// @filename: b.ts
export const valid = 1;
// @filename: a.ts
import { "missing" as x, "" as y, valid } from "./b";
function f() {
    const v = await(Promise.resolve(1));
    return v;
}
class C extends A extends B {}
class D extends E, F {}
void [x, y, valid, f, C, D];
