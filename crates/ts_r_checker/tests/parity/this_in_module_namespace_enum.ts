// `this` at a module's top level is `undefined`; in a namespace body it is TS2331 and in
// an enum member TS2332 (each with TS2683 under noImplicitThis).
namespace M {
    var x = this;
    export const y = this.z;
    function f() { return this; }
    const g = () => this;
}
enum E {
    A = this,
    B = this.spaaaace,
}
export var lam = () => this.window;
const top = this.foo;
const w = () => this;
const q: number = w();
