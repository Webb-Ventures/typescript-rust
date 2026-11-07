// A generic mapped type with known members is no subtype of an object with other
// (optional) properties, so a type predicate drops it (TypeORM's `MongoEntityManager`:
// `isFindManyOptions(o)` on `FindManyOptions<E> | Partial<E> | FilterOperators<E>`).
interface FindManyOptions<E> {
  select?: (keyof E)[];
  skip?: number;
  where?: Partial<E>;
}
interface FilterOperators<V> {
  $eq?: V;
  [k: string]: unknown;
}
declare function isFindManyOptions<E = any>(obj: any): obj is FindManyOptions<E>;

export function find<Entity>(o?: FindManyOptions<Entity> | Partial<Entity> | FilterOperators<Entity>) {
  if (isFindManyOptions(o)) {
    return o.skip;
  }
}
export function find2<Entity>(o?: FindManyOptions<Entity> | Partial<Entity>) {
  if (isFindManyOptions(o)) {
    return o.select;
  }
}
export function probe<E>(p: Partial<E>, q: FindManyOptions<any>) {
  const arr = [p, q];
  const n: number = arr;
}
export function find3<E extends { skip?: number }>(o: Partial<E> | string) {
  if (isFindManyOptions(o)) {
    const n: number = o;
  }
}
