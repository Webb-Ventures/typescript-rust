// Members typed `this` read through a `this`-typed receiver stay `this`.
class Node2 {
    next?: this;
    parent: this | null = null;
    items: this[] = [];
    map = new Map<string, this>();
    clone(): this {
        return this;
    }
    m() {
        const r: this | undefined = this.items[0];
        const s: this | undefined = this.map.get("a");
        const bad: string = this.items[0];
        return [r, s, bad];
    }
    private extracted(x: this) {
        const y: this = x.clone();
        return y.items.filter((i) => i !== this);
    }
}
class Sub extends Node2 {
    extra = 1;
    use() {
        return this.items.map((i) => i.extra);
    }
}
export { Node2, Sub };

// The type printer parenthesizes `infer U` inside an array and keeps its constraint.
export function f<T>() {
    const n: { a?: T extends (infer U)[] ? U : never } = { b: 1 };
    const m: { a?: T extends (infer V extends string)[] ? V : never } = { b: 2 };
    const k: { a?: T extends { a: infer W extends string } ? W[] : never } = { b: 3 };
    return [n, m, k];
}

// @filename: c.ts
// Statics of `export default class C` are reachable through `C` in a static block.
export default class C {
    static v = 1;
    static {
        C.v++;
        C.w;
    }
}
