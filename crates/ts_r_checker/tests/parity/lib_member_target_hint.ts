// A member of a later lib is TS2550, naming the lib (on the apparent type's interface).
export {};
declare const tup: [number, string];
tup.at(0);
declare const rtup: readonly [number, string];
rtup.at(0);
declare const u: string | number[];
u.at(0);
declare const u2: string[] | number[];
u2.at(0);
declare const i: number[] & { x: 1 };
i.at(0);
class MyArr extends Array<number> {
  m() { return this.at(0); }
}
new MyArr().at(0);
function g<T extends string>(t: T, a: T[]) {
  t.padStart(1);
  a.at(0);
}
declare const s: "lit";
s.trimEnd();
declare const n: 1;
n.toFixed(1);
Object.entries({});
Object["entries"]({});
declare const o: ObjectConstructor;
o.values({});
declare const arr: Array<number>;
arr.findLast(() => true);
arr.at;
const k = "at";
arr[k];
declare const sym: symbol;
sym.description;
interface Local { x: 1 }
declare const l: Local;
l.at;
declare const ro: ReadonlyArray<number>;
ro.at(0);
"".padStrt;
[].flat();
Intl.PluralRules;
Intl.supportedValuesOf;
declare const dv: DataView;
dv.getBigInt64(0);
declare const u8: Uint8Array;
u8.findLast(() => true);
u8.entries();
Promise.allSettled([]);
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "lib": ["es2015"], "target": "es2015", "noEmit": true, "types": [] }, "files": ["a.ts"] }
