// @target: esnext
// @module: esnext
// @experimentalDecorators: true
// @noEmitHelpers: true
declare var dec: any;
export const x = 1;
using z = { [Symbol.dispose]() {} };
await using a = null;
export const w = 3;
@dec
export class C {}
export default 4;
function f() {
    using y = null;
    for (using q of []) {}
}
