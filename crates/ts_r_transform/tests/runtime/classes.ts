// ES2022 class statics: the lowered program prints what the original prints.
const log: string[] = [];
const out = (...xs: unknown[]) => log.push(xs.map((x) => JSON.stringify(x) ?? String(x)).join(" "));

let k = "dyn";
const order: string[] = [];

class A {
    static s = 1;
    static t: number;
    x = 2;
    static {
        order.push("A block " + this.s);
    }
    static [k] = 3;
    static m() { return A.s + 10; }
}
out(A.s, A.t, new A().x, (A as any).dyn, A.m(), order);

const B = class {
    static s = 1;
    static { this.s++; }
};
out(B.s, B.name);

class C extends A {
    static u = super.m();
    static {
        order.push("C block " + super.m());
    }
}
out(C.u, order);

class D {
    static self = D;
    static f = () => this;
}
out(D.self === D, D.f() === D);

function make() {
    return class {
        static s = this;
        static n = this.name;
    };
}
const M = make();
out(M.s === M, typeof M.n);

let I = class {
    static s = 1;
};
out(I.name, I.s);

const objs = { P: class { static q = 2; } };
out(objs.P.name, objs.P.q);

class Counter {
    static count = 0;
    static inc = () => ++Counter.count;
}
Counter.inc();
Counter.inc();
out(Counter.count);

// Writes to `super` properties in static code (through `Reflect.set` below ES2022).
class SBase {
    static a = 1;
    static k = "a";
}
let sx: any;
class SDerived extends SBase {
    static z1 = super.a = 5;
    static z2 = super.a += 2;
    static z3 = ++super.a;
    static z4 = super.a++;
    static z5 = --super[SBase.k];
    static z6 = [super.a] = [20];
    static z7 = ({ x: super.a = 30 } = { x: undefined });
    static z8 = (() => { super.a = 40; return super.a; })();
    static {
        super.a *= 2;
        super.a++;
        ++super[SBase.k];
        sx = { ...super.a } = { p: 1 };
    }
}
out(SDerived.z1, SDerived.z2, SDerived.z3, SDerived.z4, SDerived.z5, SDerived.z6, SDerived.z7);
out(SDerived.z8, (SDerived as any).a, SBase.a, JSON.stringify(sx));

console.log(log.join("\n"));
