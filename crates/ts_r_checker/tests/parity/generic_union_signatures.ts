// trpc `stream/sse.ts` (`Serialize | (<T>(it: T) => T)`) and `links/splitLink.ts`
// (`asArray(x).map(...)`): single signatures combine across a union when the generic ones
// have the same type parameters; a type predicate keeps the directly related members and
// only intersects generic members when there are none.
type Ser = (value: any) => any;
declare const s: Ser | (<T>(it: T) => T);
const r = s(1);
const n: null = r;
declare const g: (<T>(it: T) => T) | (<U>(it: U, b: number) => U[]);
g(1, 2);
declare const h: ((a: string) => 1) | (<T>(it: T) => T);
const hr: null = h("x");
function asArray<TType>(value: TType | TType[]) {
  return Array.isArray(value) ? value : [value];
}
type L = (x: number) => string;
declare const l: L | L[];
const a = asArray(l);
const yes = a.map((link) => link(1));
const t: null = yes;
const at: null = a;
declare const ia: L[] | any[];
const ib = ia.map((link) => link(1));
const ic: null = ib;
declare function isArr(x: unknown): x is any[];
declare function isStr(x: unknown): x is string;
export function f<T, O extends object>(a: T | string, b: T | number[], c: O | O[], e: T | { x: 1 }) {
  if (isArr(a)) { const n: null = a; }
  if (isArr(b)) { const n: null = b; }
  if (isArr(c)) { const n: null = c; }
  if (isStr(e)) { const n: null = e; }
  if (Array.isArray(b)) { const n: null = b; }
}
