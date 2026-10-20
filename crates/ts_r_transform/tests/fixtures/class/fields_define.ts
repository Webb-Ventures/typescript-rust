// @target: es2022
// @useDefineForClassFields: true
declare class Base { constructor(...a: any[]) }
class A extends Base {
  a = 1;
  b: string;
  declare c: number;
  static s = 2;
  static t: number;
  [key: string]: any;
  #p = 3;
  ["comp"] = 4;
  accessor acc = 5;
  constructor(public x: number, private readonly y?: string) {
    "use strict";
    console.log("pre");
    super(x);
    this.a = 2;
  }
  m(): void {}
}
class B extends Base { f = 1; }
class C { constructor(public x: number) { } g = this.x; }
class D extends Base { constructor(public x: number) { if (x) { super(); } else { super(1); } } }
const E = class { constructor(protected z: number) {} };
