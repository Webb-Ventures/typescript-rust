// An inferred array of object literals widens to regular elements, so T's constraint does not check excess properties.
type DeepPartial<T> =
  | T
  | (T extends Array<infer U>
      ? DeepPartial<U>[]
      : T extends object
        ? { [K in keyof T]?: DeepPartial<T[K]> }
        : T);
class Setting {
  assetId?: number;
  asset?: User;
  name!: string;
  value!: string;
}
class User {
  id!: number;
  settings!: Setting[];
}
declare function one<T extends DeepPartial<User>>(entities: T[]): T;
export const r: number = one([{ id: 1, settings: [{ assertId: 1, name: "A", value: "x" }] }]);
export const r2: number = one([{ id: 1, settings: [{ assetId: 1, name: "A", value: "x" }] }]);
export const r3: number = one([{ id: 1, settings: [] }]);
declare function two<T extends DeepPartial<User>>(entities: T): T;
export const r4: number = two({ id: 1, settings: [{ assertId: 1, name: "A", value: "x" }] });
