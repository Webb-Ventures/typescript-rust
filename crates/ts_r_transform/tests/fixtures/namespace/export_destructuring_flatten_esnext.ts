// @target: esnext
declare var o: any, k: string, f: any;
namespace N {
    export const { a, b = 1, [k]: c, d: { e } } = o;
    export const { q } = o, s = 1;
    export const [t] = f();
    export const key = "a";
    export const source = { a: 1, b: undefined as any, pair: [3, 4] };
    export const fallback = 2;
    export const { [key]: computed, b: bb = fallback } = source;
    export const [x2, y2] = source.pair;
    export let { m: { n } = {} as any } = o;
}
namespace N2 {
    export const [[g1] = [] as any[], { h1, h2 } = {} as any] = f();
}
