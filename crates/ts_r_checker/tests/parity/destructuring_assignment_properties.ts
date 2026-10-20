export {};
var zz: number, yy: number;
declare const o: { a: number };
({ zz } = o);
({ zz: yy } = o);
({ zz = 1 } = o);
({ a: zz, b: yy } = { a: 1 });
declare const u: { a: number } | { b: number };
({ zz } = u);
declare const s: string;
({ zz } = s);
declare const uk: unknown;
({ zz } = uk);
function f(options?: { color?: string; width?: number }) {
    let color: string | undefined, width: number | undefined;
    ({ color, width } = options || {});
    return [color, width];
}
var aa: string, bb: number;
var tuple: [string, number] = ["", 1];
[...{ bb }] = tuple;
[...{ 0: aa = "", bb }] = tuple;
[...{ 0: aa }] = tuple;
[...{ 0: aa = "", bb }] = ["", 0];
for (var {x: a = 0} of [2, 3] as number[]) {
    const q1: boolean = a;
}
for (var {x: c = 0} of [2, 3]) {
    const q3: boolean = c;
}
for (var {x: d} of [2, 3]) {
    const q4: boolean = d;
}
