// @target: es2022
// Top-level `import x = A.B` in a script: kept when the target is a value or unresolved,
// dropped when it only names types or a type-only namespace.
declare namespace foo { interface T {} const v: any; namespace inner { const q: 1; } namespace tonly { interface I {} } }
namespace bar { export interface U {} }
import y = foo.T;
import z = bar;
import w = missing.baz;
import v = foo.v;
import i = foo.inner;
import t = foo.tonly;
namespace N {
  import a = foo.v;
  export import b = foo.v;
}
function f() {}
