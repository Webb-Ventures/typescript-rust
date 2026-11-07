// A conditional over a conditional check type relates through its branches before its
// distributive constraint (tRPC's tagged query keys extending options with an optional key).
declare const tagSym: unique symbol;
type AnyTag = { [tagSym]: any };
type Tag<T, V> = T extends AnyTag ? T : T & { [tagSym]: V };
type Key<P extends boolean> = P extends true ? [string[], string] : [string];
type Flags = { prefix: boolean };
interface Opts<K> { key?: K; other?: number }
type OmitKeyof<T, K extends keyof T> = Omit<T, K>;

interface A<V, F extends Flags> extends Opts<Key<F['prefix']>> {
  key: Tag<Key<F['prefix']>, V>;
}
interface B<V, F extends Flags> extends OmitKeyof<Opts<Key<F['prefix']>>, 'other'> {
  key: Tag<Key<F['prefix']>, V>;
}
interface C<V, F extends Flags> extends Partial<Opts<Key<F['prefix']>>> {
  key: Tag<Key<F['prefix']>, V>;
}
type Plain<T, V> = T extends string ? T : T & { v: V };
function f<V, P extends boolean>(x: Tag<Key<P>, V>, y: Plain<Key<P>, V>) {
  const a: Key<P> = x;
  const b: Key<P> | undefined = x;
  const c: Key<P> = y;
  const d: Key<P> | undefined = y;
}
