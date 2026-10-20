export { string };
export type { number };
export { undefined };
export { Array };
export { nope };
export { globalThis };
export { Symbol as S };
const local = 1;
export { local };
interface Window2 { }
export type { Window2 };
export { NaN };
export { console };
export { any };
export { boolean };
export { never };
export { object };
export { unknown };
export { symbol };
export { bigint };
declare module "m" {
    export { Array };
    export { local };
    export { missing };
    const local: number;
}
type A = number;
declare global {
    interface G { }
    var gv: number;
    export { A as AA, G as GG, Array as Arr, gv as GV };
    import * as self from "./a";
}
export { gv, G };
namespace NA {
    global {
        interface Array<T> { extra: T }
    }
}
