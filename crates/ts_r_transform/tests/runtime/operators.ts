// ES2016-ES2021 operators: the lowered program prints what the original prints.
const log: string[] = [];
const out = (...xs: unknown[]) => log.push(xs.map((x) => JSON.stringify(x) ?? String(x)).join(" "));

let calls = 0;
const obj: any = {
    a: { b: { c: 1 }, f(this: any, x: number) { return this === obj.a ? x * 2 : -1; } },
    n: null,
    get counted() { calls++; return this.a; },
};

out(obj.a?.b?.c, obj.n?.b, obj.missing?.b.c, obj.a?.f(3), obj.n?.f(3), obj.a.g?.(1));
out(obj.counted?.b.c, calls);
out((obj.a?.f)(4));
out(delete obj.n?.x, delete obj.a?.zz);
const deleted: any = { x: 1, y: 2 };
out(delete (deleted?.x), deleted.x, delete (((deleted?.["y"]))), deleted.y);
out(delete (obj.n?.x), delete ((obj.missing?.x)));
out(obj.n ?? "dflt", 0 ?? 1, "" ?? 2, obj.counted?.b ?? 3, calls);

let x: any = null;
x ??= 5;
x ||= 6;
x &&= 7;
const o: any = { k: 0, list: [1, 2] };
o.k ||= 10;
o.list[1] &&= 20;
o["z"] ??= 30;
out(x, o);

let e = 2 ** 3 ** 2;
e **= 0.5;
const arr = [2, 3];
let i = 0;
arr[i++] **= 2;
out(e, arr, i, (-2) ** 2, 1_000_000, 0xf_f);

try {
    throw new Error("boom");
} catch {
    out("caught");
}

function chain(this: any, v?: { w?: { (): number } }) {
    return v?.w?.() ?? "none";
}
out(chain(), chain({}), chain({ w: () => 4 }));

// Tagged templates with invalid escapes (`__makeTemplateObject` below ES2018).
function rawTag(s: TemplateStringsArray, ...v: unknown[]) {
    return JSON.stringify([s, s.raw, v]);
}
out(rawTag`\unicode ${1} \xg${2}ok\n`, rawTag`plain ${3}`, rawTag`\u{110000}`);

console.log(log.join("\n"));
