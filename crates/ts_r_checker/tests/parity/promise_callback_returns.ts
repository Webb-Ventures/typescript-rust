// A conditional source relates through its generic branches' constraints; a callback's return context keeps a defaulted type parameter open; only a single unit return type widens.
type ObjectLiteral = { [key: string]: any };
declare function findOne<E extends ObjectLiteral>(): Promise<E | null>;
type A1<T> = T extends null ? T : T;
export function f4<E extends { a: 1 }>(x1: A1<E>, y: Awaited<E>) {
  const z1: never = x1;
  const w: never = y;
}
export async function f<Entity extends ObjectLiteral>(): Promise<Entity> {
  return findOne<Entity>().then((value) => {
    if (value === null) {
      return Promise.reject(new Error("x"));
    }
    return Promise.resolve(value);
  });
}
export async function g<Entity extends ObjectLiteral>() {
  const p1 = findOne<Entity>().then((value) => {
    return Promise.reject(new Error("x"));
  });
  const q1: number = p1;
  const p4 = findOne<Entity>().then((value) => {
    if (value === null) return Promise.reject(1);
    return 1;
  });
  const q4: number = p4;
}
export function r(x: boolean) {
  if (x) return 1;
  return "a";
}
export const b: string = r;
