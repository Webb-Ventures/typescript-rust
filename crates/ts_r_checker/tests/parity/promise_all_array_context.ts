// An array the call's contextual type implies through Promise.all's mapped return type is no tuple context; comparable unions elaborate against their last member.
type ObjectLiteral = { [key: string]: any };
class Post {
  id!: number;
  title!: string;
}
declare function getOne(): Promise<ObjectLiteral | null>;
export async function f(t: [ObjectLiteral | null, ObjectLiteral | null]) {
  const a = (await Promise.all([getOne(), getOne()])) as Post[];
  const b = Promise.all([getOne(), getOne()]) as Promise<Post[]>;
  const c: Post[] = await Promise.all([getOne(), getOne()]);
  const e = (await Promise.all([getOne(), getOne()])) as number;
  const g = t as Post[];
  const [x, y] = (await Promise.all([getOne(), getOne()])) as [Post, Post];
}
export function h<T>(v: number) {
  if (v) return [] as T[];
  return [1, 2] as [number, number];
}
