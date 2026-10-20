// `Model.save({ name })` with `this: { new (): T }` and `DeepPartial<T>`: the class
// candidate wins over the object literal one, so `T` is the class.
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
declare class Base {
  hasId(): boolean;
  static save<T extends Base>(this: { new (): T } & typeof Base, e: DeepPartial<T>): T;
}
declare class User extends Base {
  id: number;
  name: string;
}
const r = User.save({ name: "x" });
export const n: number = r;
declare function save2<T extends Base>(c: { new (): T }, e: DeepPartial<T>): T;
export const n2: number = save2(User, { name: "x" });
declare function save3<T>(c: { new (): T }, e: T | { [K in keyof T]?: T[K] }): T;
export const n3: number = save3(User, { name: "x" });
declare function save4<T>(c: { new (): T }, e: T | (T extends object ? { [K in keyof T]?: T[K] } : T)): T;
export const n4: number = save4(User, { name: "x" });
