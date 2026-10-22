// An array of tuples the call's contextual type implies for a type parameter keeps the
// array literal argument's elements tuples, also through Promise.all's mapped return type.
declare function all1<T extends readonly unknown[] | []>(values: T): Promise<{ -readonly [P in keyof T]: Awaited<T[P]> }>;
declare function all2<T extends readonly unknown[] | []>(values: T): Promise<{ -readonly [P in keyof T]: T[P] }>;
declare function all3<T extends readonly unknown[] | []>(values: T): { -readonly [P in keyof T]: T[P] };
declare function all4<T extends readonly unknown[] | []>(values: T): T;
declare function all5<T extends readonly unknown[]>(values: T): T;
const e1: Promise<[string, number][]> = all1([["", 1]]);
const e2: Promise<[string, number][]> = all2([["", 1]]);
const e3: [string, number][] = all3([["", 1]]);
const e4: [string, number][] = all4([["", 1]]);
const e5: [string, number][] = all5([["", 1]]);
async function g(xs: string[]) {
  const e4: [string, number][] = await Promise.all(xs.map((e) => [e, 1]));
  const e5: [string, number][] = await Promise.all([["", 1]]);
  const e6: string[][] = await Promise.all([["", 1]]);
}
export {};
