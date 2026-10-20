// x! where x narrowed to only null or undefined reads the declared type (not through parentheses).
class Sub {
  public a?: { x?: string };
  public b?: { x: string };
  public c: { x?: string } | undefined;
}
declare function expect(x: unknown): void;
export function f(s: Sub) {
  s.a = undefined;
  expect(s.a!.x);
  s.b = undefined;
  expect(s.b!.x);
  s.c = undefined;
  expect(s.c!.x);
  let l: { x?: string } | undefined;
  l = undefined;
  expect(l!.x);
  const t1: number = s.a;
  let m: string | null = null;
  const t2: number = m!;
  const t3: number = (m)!;
}
