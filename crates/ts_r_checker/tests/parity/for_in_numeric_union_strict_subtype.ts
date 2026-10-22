// `for (k in o) o[k]` reads a number index only when `o` has just a number index
// signature; a union has the signatures whose key every member declares exactly.
declare const a: any[] | Record<string, any>[];
declare const b: any[] | Record<string, any>;
declare const c: number[] | { [k: string]: number; [n: number]: number };
for (let k in a) a[k];
for (let k in b) b[k];
for (let k in c) c[k];
// `any[]` is no strict subtype of `Record<string, any>`, so the join keeps it.
function f(obj: Record<string, any> | Record<string, any>[]) {
  if (!(Array.isArray(obj) && obj.length)) {
    const t: 1 = obj;
    for (let key in obj) obj[key];
  }
}
export {};
