namespace M {
    export const x = 0;
}
M.x = 1;
M.x++;
--M.x;
++((M.x));
M.x += 1;
interface X { readonly a: number }
declare const o: X;
o.a++;
o.a = 2;
for (o.a of [1]) {}
for ((o.a) in {}) {}
class C {
    readonly a: number = 1;
    constructor(readonly p: number) {
        const f = () => { this.a = 1; this.a++; };
        (() => { this.a = 1; this.a++; this.p = 2; })();
        this.p = 3;
    }
}
new C(1).p = 2;
class B extends C {
    constructor(x: number) { super(x); this.p = 1; }
}
enum E { A, B }
++E["B"];
E["B"]++;
E.A--;
export var y = 1;
import * as self from "./a";
import { y as yy } from "./a";
self.y = 2;
self.y++;
self["y"] = 3;
(self.y) = 4;
for (self.y of []) {}
const copy = self;
copy.y = 5;
// Element-access and parenthesized update operands, constructor arrows, and a class's
// `prototype` (writable).
namespace M2 {
    export const x = 0;
}
--M2["x"];
(M2.x)--;
enum E2 { A, B }
E2.B++;
class C2 {
    readonly a: number;
    constructor() {
        this.a = 1;
        const f = () => {
            this.a = 2;
        };
        (() => {
            this.a = 3;
        })();
        f();
    }
}
C2.prototype = new C2();
declare const o2: { readonly n: number };
for (o2.n of [1, 2]) {}
