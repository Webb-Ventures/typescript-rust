// A generic method of a class or interface gets copies of its type parameters when the
// type is instantiated; a default naming a sibling (`B = A`) names the copy, so an
// uninferred `B` falls back to what `A` became.
interface Opts<A = unknown, B = A, K = string> {
  key?: K;
  b?: B;
}
declare class Cache<T> {
  build<A = unknown, B = A, K = string>(options: Opts<A, B, K>): [A, B, K, T];
}
interface State<D> {
  data: D | undefined;
}
declare class Entry<A = unknown, B = A> {
  setState(state: Partial<State<B>>): void;
  a?: A;
}
interface Store {
  make<A = unknown, B = A>(options: { b?: B }): Entry<A, B>;
}
export function f(c: Cache<number>, s: Store) {
  const r = c.build({ key: "x" });
  const n1: number = r;
  const e = s.make({});
  e.setState({ data: [] });
  const n2: number = e;
}
