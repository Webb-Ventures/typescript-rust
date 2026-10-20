// `delete` through a readonly index signature writes it: TS2542 at the element access.
export {};
declare var v: readonly [number, number, ...number[]];
delete v[2];
delete v[0 + 1];
declare var r: { readonly [k: string]: number | undefined };
delete r["a"];
declare var w: { [k: string]: number | undefined };
delete w["a"];
