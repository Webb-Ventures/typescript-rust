// @target: es2015
// @module: esnext
// @noEmitHelpers: true
import { y } from "./other";
console.log(0);
export function f() { }
function g() { }
export { a as aa };
export { D, _default as default };
var a, u, D, _default, v, E, NS;
export let x, p, r;
const env_1 = { stack: [], error: void 0, hasError: false };
try {
    a = __addDisposableResource(env_1, null, false);
    x = 1, { p, q: [r] } = y;
    D = class D {
    };
    _default = 42;
    v = 3;
    (function (E) {
        E[E["A"] = 0] = "A";
    })(E || (E = {}));
    (function (NS) {
        NS.z = 1;
    })(NS || (NS = {}));
}
catch (e_1) {
    env_1.error = e_1;
    env_1.hasError = true;
}
finally {
    __disposeResources(env_1);
}
