// @target: es2015
// @noEmitHelpers: true
declare var dec: any;

@dec
class C {
    @dec accessor z = 1;
    @dec static accessor #z = 1;
}
