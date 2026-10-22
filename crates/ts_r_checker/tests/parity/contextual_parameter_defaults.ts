// A contextually typed parameter's default must fit the contextual type; a shorthand
// assignment default that may be undefined keeps the read value's undefined.
var f4: (a: number) => void = function (a = "") { };
var f5: (a: number) => void = (a = "") => { };
declare function take(cb: (a: number, b: string) => void): void;
take((a = 1, b = 2) => {});
take(function (a = "x") {});
let x: number = 1;
declare const a: { x?: number };
({ x = undefined } = a);
({ x: x = undefined } = a);
const o2 = {
  get p1() { return 0; },
  set p1(value: string) {},
};
function foo(a: any) {
  arguments = 10;
}
