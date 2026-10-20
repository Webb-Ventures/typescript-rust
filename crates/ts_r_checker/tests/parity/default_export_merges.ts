// A named default export merged with other declarations sharing a meaning: TS2652.
export default function A() {}
namespace A { export var x = 1; }
export namespace A { export var y = 1; }
namespace A { interface I {} }
export interface A {}

declare module "m" {
    export default class B {}
    interface B { b: number }
    namespace B { interface J {} }
}
declare module "n" {
    export default interface C { a: number }
    namespace C { export const x = 1; }
    type C = number;
}
