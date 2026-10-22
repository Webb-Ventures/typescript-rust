// `{}` stays next to a non-unit string, number or bigint (`string & {}`), so a union keeps
// its literals beside it; it keys an index signature and has the primitive's members.
const a1: undefined = null! as string & {};
const a2: undefined = null! as "a" & {};
const a3: undefined = null! as number & {};
const a4: undefined = null! as { a: 1 } & {};
const a5: undefined = null! as boolean & {};
const a6: undefined = null! as `a${string}` & {};
const a7: undefined = null! as bigint & {};
const a8: undefined = null! as string & {} & { x: 1 };

type Alignment = (string & {}) | "left" | "center" | "right";
const x: undefined = null! as Alignment;
type Alignments = Record<Alignment, string>;
declare const m: Alignments;
const l: undefined = m.left;
const o: undefined = m.other;
type K = keyof Alignments;
const k: undefined = null! as K;
const n: number = (null! as string & {}).length;
const ok: Alignment = "anything";

function f1<T extends string | undefined>(x: T, y: { a: T }) {
    if (x) {
        x.length;
    }
    if (y.a) {
        y.a.length;
    }
}
const b1: undefined = null! as (string | undefined) & {};
const b2: undefined = null! as (string | number | Date) & {};
type NN<T> = T & {};
const b4: undefined = null! as NN<string>;
const b5: undefined = null! as NN<string | null>;
const b6: undefined = null! as NonNullable<string>;
const b7: undefined = null! as NonNullable<string | null>;
function f2<T extends string | undefined>(x: T) {
    if (x) {
        const y: undefined = x;
    }
}
