// Declaration merging that is allowed and conflicts that are not.
interface I { a: number }
interface I { b: string }
namespace N { export const x = 1; }
namespace N { export const y = 2; }
function F() {}
namespace F { export const z = 3; }
class C {}
interface C { extra: number }
enum E { A }
enum E { B = 2 }
let dup = 1;
var dup = 2;
class Twice {}
class Twice {}
type T = number;
interface T {}
const enum CE { X }
enum CE { Y }
