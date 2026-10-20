// @target: es2022
// @experimentalDecorators: true
// @emitDecoratorMetadata: true
import { Imp, type TI } from "./imp";
declare const dec: any;
enum En { A }
class Loc {}
interface If {}
export class A {
  @dec p1: number;
  @dec p2: string | null;
  @dec p3: Loc;
  @dec p4: Imp;
  @dec p5: If;
  @dec p6: TI;
  @dec p7: En;
  @dec p8: () => void;
  @dec p9: string[];
  @dec p10: "lit";
  @dec p11: boolean;
  @dec p12: any;
  @dec p13: Promise<number>;
  @dec p14: bigint;
  @dec p15: symbol;
  @dec static s1 = 1;
  @dec m(@dec a: number, b: Loc): string { return ""; }
  @dec get g(): number { return 1; }
  @dec set g2(v: string) {}
  @dec async am(): Promise<void> {}
  @dec ["comp"]() {}
}
@dec export default class B { constructor(x: Imp) {} }
