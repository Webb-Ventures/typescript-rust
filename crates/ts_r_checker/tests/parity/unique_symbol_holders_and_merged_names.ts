interface Registry {
    readonly key: unique symbol;
}
declare var Registry: Registry;
const k = Registry.key;
let n: number = k;
let m: number = Registry.key;

class Path {
    static make() {
        let u: number = Path.Utils;
        return u;
    }
}
namespace Path {
    export class Utils {}
}
namespace Path {
    let w: number = Utils;
}
export { k, Path };
interface Holder {
    readonly tag: unique symbol;
}
declare const h: Holder;
let t: number = h.tag;
