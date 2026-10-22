// `++`/`--` through a readonly index signature is TS2542, also next to TS2356 and for an
// `any` or unresolved key.
enum ENUM1 { A, B, "" };
declare let k: any;
ENUM1[k]--;
ENUM1[k] = 1;
declare const ro: { readonly [x: string]: number };
ro[k] = 1;
ro[k]++;
ro[k] += 1;
ENUM1[Q]--;
declare const arr: readonly number[];
arr[0]++;
