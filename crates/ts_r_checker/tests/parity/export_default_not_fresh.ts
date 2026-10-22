// An imported default-exported object literal is no longer fresh: a missing property is
// reported where it is used.
import B from "./items";
interface IFoo {
    foo: string;
}
function nFoo(x: IFoo) {}
nFoo(B);
const c: IFoo = B;
// @filename: items.ts
export default {
    foob: "a"
}
