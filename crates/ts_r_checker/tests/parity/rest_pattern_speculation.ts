// Destructured locals in a context-sensitive arrow are re-typed per inference pass, and a
// generic method override whose type parameter breaks the base's constraint is inferred.
interface Obs<T> {
  value: T;
}
declare function map<T, R>(src: Obs<T>, fn: (value: T, index: number) => R): Obs<R>;
declare const o: Obs<{ num: number }>;

const r = map(o, (...args) => {
  const [value] = args;
  return value.num;
});
export const bad: string = r;

type Key<K, T> = T extends object ? keyof T : K;
declare class Emitter<T> {
  on<K>(eventName: Key<K, T>, listener: (...args: any[]) => void): this;
}
interface Events {
  data: [string];
  end: [];
}
export class Stream extends Emitter<Events> {
  on<U extends keyof Events>(event: U, listener: (...args: Events[U]) => void): this {
    return this;
  }
}
