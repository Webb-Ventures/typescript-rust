function f1() {
    let x: never;
    x = {};
    x();
}
function f2(y: never) { y(); }
declare const z: never;
z();
class CtorTag {}
CtorTag `a`;
declare var s: string;
s `x`;
const arr = [`x` `y`];
declare const fn: Function;
fn `ok`;
export {};
