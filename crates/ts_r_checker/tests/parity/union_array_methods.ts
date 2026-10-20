// date-fns `renderFormatParse`: a method of a union of arrays whose signatures don't
// combine is the method of an array of the union of the elements; callbacks of an
// intersection of function types get the union of their parameters.
declare const a: (number | Date)[] | Date[];
const r = a.map((d, i) => String(d) + i);
const r2: string[] = r;
declare const b: string[] | number[];
const f = b.filter((x) => !!x);
const f2: number = f;
b.forEach((x) => x);
const red = b.reduce((acc, x) => acc, 0);
const s = b.some((x) => x);
const j = b.find((x) => x);
const k: number = j;
declare const ro: readonly string[] | number[];
const m = ro.map((x) => x);
declare function g(cb: ((v: string) => void) & ((v: number, w: boolean) => void)): void;
g((v) => {});
g((v, w) => {});
export {};
