// A namespace used as a type is TS2709, whether or not it is instantiated (a value too).
namespace Values { export var x = 1; }
namespace Types { export interface I {} }
declare namespace Ambient { var v: number; }
var a: Values;
var b: Types;
var c: Ambient;
class C implements Types {}
function f(p: Values): Ambient { return p; }
var ok: Types.I;
var nope: Missing;
