declare const p: Promise<number>;
export class A {
  x = await p;
}
export async function f() {
  class B {
    x = await p;
    static y = await p;
    [await p] = 1;
  }
  enum E {
    a = await p,
  }
  return [B, E];
}
export function* g(): Generator<number, unknown, string> {
  class C {
    [yield 1] = yield 2;
  }
  return C;
}
declare function dec(a: unknown): any;
export function h(value: Promise<number>) {
  class D {
    @dec(await value) async m() {}
  }
  return D;
}
