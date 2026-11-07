// TS2564 follows control flow to every return of the constructor, and TS2565 reports
// `this.p` read in the constructor before every path assigns it (not as the target of a
// logical assignment).
declare function f(): boolean;
class A1 {
    a: number; b: number; c: number; d: number; e: number; g: number; h: number;
    i: number; j: number; k: number; l: number; m: number; n: string; o: number;
    #p: number;
    constructor(x: number, s: "x" | "y") {
        if (f()) { this.a = 1; } else { this.a = 2; }
        [this.b] = [1];
        ({ c: this.c } = { c: 1 });
        try { this.d = 1; } catch { this.d = 2; }
        for (const _ of [1]) { this.e = 1; }
        this.g = 0;
        while (f()) { this.g++; }
        this.h = 0; this.h += this.h;
        switch (s) { case "x": this.i = 1; break; case "y": this.i = 2; break; }
        if (!f()) throw new Error();
        this.j = 1;
        const cb = () => this.k;
        this.k = 1;
        this.l = x > 0 ? 1 : 2;
        this.m = this.l;
        this.n = typeof this.o;
        this.o = 1;
        this.#p = 1;
        do { this.#p++; } while (f());
    }
}
class A2 {
    a: number;
    b: number;
    c: number;
    d: number;
    e: number;
    constructor() {
        this.a += 1;
        this.b ||= 1;
        this.c &&= 1;
        this.d++;
        this.e = (this.e ?? 0) + 1;
    }
}
class A3 {
    a: number;
    #b: number;
    constructor(cond: boolean) {
        if (cond) {
            return;
        }
        this.a = 0;
        this.#b = 0;
    }
}
class Base { x = 1 }
class A4 extends Base {
    a: number;
    b: number;
    constructor() {
        super();
        this.a = this.x;
        try { f(); } finally { this.b = 1; }
    }
}
class A5 {
    ["c"]: number;
    ["d"]: number = 1;
    [Symbol.iterator]: number;
    constructor() { }
}
