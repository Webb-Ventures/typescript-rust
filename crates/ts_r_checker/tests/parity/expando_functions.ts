// Expando properties: assignments to a function's (or a const function variable's)
// properties in the same function or file declare them (`Comp.displayName = "x"`).
function Foo() {}
Foo.a = 1;
Foo.a = "x";
Foo["b"] = true;
if (Math.random()) {
    Foo.inBlock = [1];
}
function nested(p = (Foo.inParam = 1)) {
    Foo.inBody = 2;
    return p;
}
Foo.handler = (x) => x;
const n: number = Foo;
Foo.later;
Foo.inParam;

export const Button = (props: { label: string }) => props.label;
Button.displayName = "Button";
Button.defaultProps = { label: "ok" };
const s: string = Button.displayName;
const label: number = Button.defaultProps.label;

let notConst = () => 1;
notConst.p = 1;

function outer() {
    function inner() {}
    inner.z = 1;
    const i: number = inner;
    return i;
}

function Merged() {}
namespace Merged {
    export const declared = 1;
}
Merged.declared = 2;
Merged.extra = "e";
const e: number = Merged.extra;

export { s, label, nested, outer, e };
