// A union member that only infers weakly (through a conditional's branches) still goes
// to the naked type parameter.
type DeepPartial<T> =
  | T
  | (T extends Array<infer U>
      ? DeepPartial<U>[]
      : T extends Map<infer K, infer V>
        ? Map<DeepPartial<K>, DeepPartial<V>>
        : T extends Set<infer M>
          ? Set<DeepPartial<M>>
          : T extends object
            ? { [K in keyof T]?: DeepPartial<T[K]> }
            : T);
declare function mergeDeep<T>(target: T, ...sources: (DeepPartial<T> | undefined)[]): T;
const r1 = mergeDeep(1, 2);
export const n1: string = r1;
const r2 = mergeDeep(2, 1, 2);
export const n2: string = r2;
class Foo { x = 1 }
const foo = new Foo();
const r3 = mergeDeep({}, { foo });
export const n3: string = r3.foo;
declare function simple<T>(target: T, ...sources: (T | undefined)[]): T;
export const n4: string = simple(1, 2);
