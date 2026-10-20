// ES2017/ES2018 async functions, async generators, `for await`, object spread and rest.
const log: string[] = [];
const out = (...xs: unknown[]) => log.push(xs.map((x) => JSON.stringify(x) ?? String(x)).join(" "));
const tick = <T>(v: T) => new Promise<T>((r) => setTimeout(() => r(v), 1));

async function plain(a: number, b = a + 1, ...rest: number[]) {
    const x = await tick(a);
    return [x, b, rest.length, arguments.length];
}

const arrow = async (p = 2, { q }: { q: number } = { q: 3 }) => (await tick(p)) * q;

class Base {
    v = 1;
    m(k: number) { return this.v + k; }
}
class Sub extends Base {
    async viaSuper() {
        const viaArrow = () => super.m(10);
        return (await tick(super.m(1))) + viaArrow();
    }
    async assignSuper() {
        super.v = 5;
        const key = "v";
        return [this.v, super[key]];
    }
}

async function* gen(n: number) {
    for (let i = 0; i < n; i++) {
        yield await tick(i);
    }
    yield* [10, 11];
    return "done";
}

async function* delegating() {
    const r = yield* gen(2);
    yield r;
}

async function consume() {
    const seen: unknown[] = [];
    for await (const v of delegating()) {
        seen.push(v);
    }
    for await (const [a, b] of [[1, 2], tick([3, 4])]) {
        seen.push(a + b);
    }
    outer: for await (const v of gen(5)) {
        for (;;) {
            if (v === 2) break outer;
            break;
        }
        seen.push("v" + v);
    }
    return seen;
}

async function errors() {
    async function* failing() {
        yield 1;
        throw new Error("gen failed");
    }
    try {
        for await (const v of failing()) {
            out("got", v);
        }
    } catch (e) {
        out("caught", (e as Error).message);
    }
}

function restAndSpread() {
    const base = { a: 1, b: 2, c: 3 };
    const { a, ...others } = base;
    const copy = { ...others, d: 4, ...{ e: 5 } };
    const { b: { ...inner }, ...outerRest }: any = { b: { x: 1 }, y: 2 };
    let r1: any, r2: any;
    ({ a: r1, ...r2 } = base);
    const params = ({ c, ...more }: any, [first, { ...tail }]: any) => [c, more, first, tail];
    out(a, others, copy, inner, outerRest, r1, r2, params(base, [9, { z: 1 }]));
    for (const { a: aa, ...left } of [base]) out(aa, left);
    // Defaults around a nested rest, and elements after a flattened one.
    let d1: any, d2: any;
    ({ x: { a: d1, ...d2 } = { a: 7, z: 8 } } = {} as any);
    const { y: { ...d3 } = base } = {} as any;
    const [{ ...d4 } = { q: 1 }, d5 = d4, [d6] = [6]]: any[] = [];
    let d7: any, d8: any;
    [{ ...d7 }, d8 = 2] = [{ w: 1 }];
    const nested = ({ k: { ...r } = {} }: any) => r;
    out(d1, d2, d3, d4, d5, d6, d7, d8, nested({}), nested({ k: { v: 1 } }));
}

async function main() {
    out(await plain(1), await plain(1, 5, 6, 7));
    out(await arrow(), await arrow(4, { q: 5 }));
    const s = new Sub();
    out(await s.viaSuper(), await s.assignSuper());
    out(await consume());
    await errors();
    restAndSpread();
    const it = gen(1);
    out(await it.next(), await it.next(), await it.next(), await it.next(), await it.next());
    console.log(log.join("\n"));
}
main();
