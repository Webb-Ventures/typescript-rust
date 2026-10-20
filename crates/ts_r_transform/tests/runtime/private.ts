// Private names: the lowered program prints what the original prints.
const log: string[] = [];
const out = (...xs: unknown[]) => log.push(xs.map((x) => JSON.stringify(x) ?? String(x)).join(" "));

class Counter {
    #count = 0;
    #step: number;
    static #made = 0;
    static #id() { return ++Counter.#made; }
    readonly id: number;

    constructor(step: number) {
        this.#step = step;
        this.id = Counter.#id();
    }
    #bump() { this.#count += this.#step; return this; }
    get #twice() { return this.#count * 2; }
    set #twice(v: number) { this.#count = v / 2; }
    tick() { return this.#bump().#count; }
    twice() { return this.#twice; }
    reset(v: number) { this.#twice = v; return this.#count; }
    post() { const old = this.#count++; return [old, this.#count]; }
    pre() { return ++this.#count; }
    static has(o: object) { return #count in o; }
    static made() { return Counter.#made; }
    other(o: Counter) { o.#count -= 1; return o.#count; }
}

const a = new Counter(2);
const b = new Counter(5);
out(a.tick(), a.tick(), b.tick(), a.twice(), a.reset(10), a.post(), a.pre());
out(Counter.has(a), Counter.has({}), Counter.made(), a.id, b.id, a.other(b));
try {
    (Counter.prototype as any).tick.call({});
} catch (e) {
    out("brand check", e instanceof TypeError);
}

class Box {
    #value = "v";
    static #shared = 1;
    read() { return this.#value + Box.#shared; }
    write(v: string) { this.#value = v; Box.#shared++; return this.read(); }
}
const box = new Box();
out(box.read(), box.write("w"), new Box().read());

const Anon = class {
    #secret = 42;
    static #tag = "anon";
    static tag() { return this.#tag; }
    reveal() { return this.#secret; }
    call() { return this.#fn(1, 2); }
    #fn(x: number, y: number) { return x + y + this.#secret; }
};
out(new Anon().reveal(), Anon.tag(), new Anon().call());

class Pair {
    #a = 1;
    #b = 2;
    static #n = 0;
    swap() {
        [this.#a, this.#b] = [this.#b, this.#a];
        ({ n: Pair.#n = 5 } = {} as { n?: number });
        return [this.#a, this.#b, Pair.#n];
    }
    take(o: Pair) {
        ({ x: o.#a, y: [o.#b] } = { x: 7, y: [8] });
        return [o.#a, o.#b];
    }
}
out(new Pair().swap(), new Pair().take(new Pair()));

function tagged(this: unknown, s: TemplateStringsArray, v: number) { return s.raw.join("|") + v; }
class Tags {
    #t = tagged;
    use() { return this.#t`a${1}b`; }
}
out(new Tags().use());

console.log(log.join("\n"));
