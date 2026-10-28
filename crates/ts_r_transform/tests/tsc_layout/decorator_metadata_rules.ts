// @target: es2015
// @module: commonjs
// @strict: true
// @experimentalDecorators: true
// @emitDecoratorMetadata: true
// @noEmitHelpers: true
import { A } from "./missing-a";
import { B } from "./missing-b";
import { R } from "./missing-r";
declare var dec: any;
export class C {
    @dec big!: bigint;
    @dec get x(): A { return null!; }
    set x(v) {}
    @dec m(...rest: R[]) {}
    @dec u!: B | null;
}
