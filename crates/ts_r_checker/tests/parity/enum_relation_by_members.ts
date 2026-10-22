// Two same-named regular enums relate only when every source member exists in the target
// with the same value; an ambient member's unknown value only clashes with a string.
namespace First { export enum E { a, b, c } }
namespace Wide { export enum E { a, b, c, d } }
namespace Narrow { export enum E { a, b } }
namespace Shifted { export enum E { c, d } }
namespace Const { export const enum E { a, b, c } }
namespace Str { export enum E { a = "a", b = "b", c = "c" } }
declare namespace Amb { export enum E { a, b, c } }
declare let first: First.E;
declare let wide: Wide.E;
declare let narrow: Narrow.E;
declare let shifted: Shifted.E;
declare let k: Const.E;
declare let str: Str.E;
declare let amb: Amb.E;
first = wide;
first = narrow;
first = shifted;
first = k;
narrow = first;
first = amb;
amb = first;
str = amb;
let one: First.E.a = Shifted.E.c;
let two: First.E.a = Narrow.E.a;
