// A homomorphic mapped type whose template does not read T reverse-maps the source's keys to unknown; array literals in its context are tuples.
interface ColOpts {
  type?: string;
  query?: (alias: string) => string;
}
interface Opts<T> {
  name: string;
  columns: { [P in keyof T]?: ColOpts };
}
declare class Schema<T = any> {
  constructor(options: Opts<T>);
}
export const A = new Schema({
  name: "a",
  columns: {
    k1: { type: "int" },
    v: { query: (alias) => `x ${alias}` },
  },
});
export const n: number = A;
declare function mk<T>(o: Opts<T>): T;
export const B = mk({
  name: "a",
  columns: {
    v: { query: (alias) => `x ${alias}` },
  },
});
export const m: number = B;
type M<T> = { [P in keyof T]?: ColOpts };
declare function mk3<T>(o: M<T>): T;
export const D = mk3({ v: { type: "x" }, w: undefined });
export const m3: number = D;
declare function mk4<T>(o: M<T>): T;
export const E = mk4([{ type: "x" }]);
export const m4: number = E;
