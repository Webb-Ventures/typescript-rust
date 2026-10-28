// @module: commonjs
// @target: esnext
namespace A { export const x = 1; }
namespace A { export const y = 2; }
enum E { a }
enum E { b = 2 }
export { A, A as B, E };
export { E as default };
class K {}
namespace K { export const x = 1; }
function G() {}
namespace G { export const y = 1; }
namespace M { export const z = 1; }
export { K, G, M as "s p" };
