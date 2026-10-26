// @target: es2015
class c4 { foo() {} }
enum c4 { One }
namespace x { export let y = 123; }
enum x { z = 1 }
function f() {}
namespace f { export const v = 1; }
declare var console: any;
namespace console { export const log2 = 1; }
const enum K { A }
namespace K { export const b = 1; }
namespace T {
    var x = 2;
    namespace x { export class Bar {} }
    namespace z { var t; }
    var z;
    namespace y { var b; }
    class y {}
    var w;
    namespace w {}
}
