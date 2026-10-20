// Computed member names: key types (TS2464), `this` (TS2465) and `super` (TS2466).
declare const b: boolean;
declare const f: () => void;

class C {
    [b]() {}
    static [{}]() {}
    get [f]() { return 1; }
}

interface I {
    [Symbol.keyFor]: string;
}

class Base {
    bar() { return 0; }
}

class D extends Base {
    [this.bar()]() {}
    [super.bar()]() {}
    m() {
        const o = { [super.bar()]: 1, [this.bar()]: 2 };
        return class {
            [super.bar()]() {}
        };
    }
    constructor() {
        super();
        const o = { [(super(), "p")]: 1 };
        () => ({ [(super(), "q")]: 1 });
    }
}
export {};
