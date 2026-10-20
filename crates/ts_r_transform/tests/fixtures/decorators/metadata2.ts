// @target: es2022
// @experimentalDecorators: true
// @emitDecoratorMetadata: true
declare const dec: any;
enum S { A = "a" }
class X {}
export class A {
  @dec p1: string | undefined;
  @dec p2: "a" | "b";
  @dec p3: string | null;
  @dec p4: X | null;
  @dec p5: S;
  @dec p6: Date;
  @dec p7: Map<string, number>;
  @dec p8: readonly string[];
  @dec p9: keyof X;
  @dec p10: null;
  @dec p11: never;
  @dec m(...rest: string[]): void {}
  @dec n(x?: number, y = 2) {}
}
export class NoCtor {}
@dec export class B {}
@dec class C2 { static make() { return new C2(); } }
