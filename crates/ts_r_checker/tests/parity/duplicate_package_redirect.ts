// tsc's package deduplication: `x@1.2.3` installed under both `a` and `b` loads once (the
// first one reached), so the classes with a private member are the same; `c`'s `x@1.2.4`
// is another package. The redirected copy is not even checked (it does not parse).
// @filename: node_modules/a/index.d.ts
import X from "x";
export function a(x: X): void;
// @filename: node_modules/a/node_modules/x/index.d.ts
export default class X {
    private x: number;
}
// @filename: node_modules/a/node_modules/x/package.json
{ "name": "x", "version": "1.2.3" }
// @filename: node_modules/b/index.d.ts
import X from "x";
export const b: X;
// @filename: node_modules/b/node_modules/x/index.d.ts
content not parsed
// @filename: node_modules/b/node_modules/x/package.json
{ "name": "x", "version": "1.2.3" }
// @filename: node_modules/c/index.d.ts
import X from "x";
export const c: X;
// @filename: node_modules/c/node_modules/x/index.d.ts
export default class X {
    private x: number;
}
// @filename: node_modules/c/node_modules/x/package.json
{ "name": "x", "version": "1.2.4" }
// @filename: a.ts
import { a } from "a";
import { b } from "b";
import { c } from "c";
a(b);
a(c);
