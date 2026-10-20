// A type predicate on an optional chain narrows the chain's object to non-nullish.
type X = { y?: { z?: string } };
declare const x: X;
declare function isNotNull<A>(v: A): v is NonNullable<A>;
declare function isNil(v: unknown): v is undefined | null;
declare function isAny(v: unknown): v is unknown;
function title(s: string) { return s; }

isNotNull(x?.y?.z) ? title(x.y.z) : null;
if (!isNil(x?.y?.z)) {
    title(x.y.z);
}
if (isNil(x?.y?.z)) {
    x.y.z;
}
if (isAny(x?.y)) {
    x.y.z;
}
