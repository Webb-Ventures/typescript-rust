// @target: es2022
// @module: esnext
// An enum merged across blocks of one namespace: each block declares it, members of earlier
// blocks fold (before outer names), and `N.E.A` folds (esbuild oracle: TSSiblingEnum).
export enum t { y, yy = y }
export enum t { z = y + 1 }
declare let y: any, z: any;
export namespace foo { export enum x { y, yy = y } }
export namespace foo { export enum x { z = y + 1 } }
export namespace foo.x {
  console.log(y, z);
}
export namespace n { export enum a { b = 100 } }
export namespace n {
  export enum x {
    c = n.a.b,
    d = c * 2,
    e = x.d ** 2,
    f = x['e'] / 4,
  }
}
export namespace n {
  export enum x { g = f >> 4 }
  console.log(a.b, n.a.b, x.g, n.x.g);
}
export namespace p.q { export const v = 1; }
export namespace p.q { console.log(v); }
