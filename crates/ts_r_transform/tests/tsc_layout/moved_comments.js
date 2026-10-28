var __classPrivateFieldGet = (this && this.__classPrivateFieldGet) || function (receiver, state, kind, f) {
    if (kind === "a" && !f) throw new TypeError("Private accessor was defined without a getter");
    if (typeof state === "function" ? receiver !== state || !f : !state.has(receiver)) throw new TypeError("Cannot read private member from an object whose class did not declare it");
    return kind === "m" ? f : kind === "a" ? f.call(receiver) : f ? f.value : state.get(receiver);
};
var _S_p;
// @target: es2015
// @module: esnext
var E;
(function (E) {
    E[E["A"] = 0] = "A"; /* before the comma */
    E[E["B"] = 1] = "B";
    /** leads C */
    E[E["C"] = 2] = "C"; // last member
})(E || (E = {}));
class S {
    constructor() {
        _S_p.set(this, "p"); // private field
        log(__classPrivateFieldGet(this, _S_p, "f"));
    }
}
_S_p = new WeakMap();
S.x = 1; // static field
(() => {
    // inside a static block
    log(S.x);
})();
/** first line has trailing spaces
 * and more
 */
function f() {
    if (S) { // after the brace
    }
    try {
        log(1);
        /* before the brace */ }
    finally { }
    return this.
        f().
        g();
}
export { E, S, f };
