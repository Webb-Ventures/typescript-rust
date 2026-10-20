// TS2454 follows the flow graph for every declarator (initialized ones too), compound
// assignments and `++` leave a reference's flow type alone, logical assignments read
// without TS2454, and an immediately invoked function runs inline.
declare function n(): number | string;
declare function f(): number;
x1;
var x1 = 1;
try {
    var a = f();
} catch {
}
a;
let c: number;
c += 1;
c;
let d: number;
d++;
d;
let g: string;
g ??= "a";
g;
let h: string;
h &&= "a";
h;
let y: string | number = 1;
y += "a";
const k1: number = y;
declare let k: 0 | 1;
k += 1;
let z: string | number = n();
if (typeof z === "number") {
    z += "x";
}
function loop() {
    let foo = 0;
    do {
        foo++;
    } while (function () { return false; }());
    return foo;
}
