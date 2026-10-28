// @target: es2022
declare let dec: any, f: any;
{ class C { static [f()] = @dec class {}; } }
const o = { [f()]: @dec class {} };
{ class C { @dec static x = @dec class {}; } }
