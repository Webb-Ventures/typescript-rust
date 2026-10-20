// A type parameter constrained to `Exclude<keyof T, X>` is a `keyof T`.
interface ZT { _zod: { def: { type: string; checks: number; error: string; extra: boolean } } }
interface ZU { type: string; checks: number }
type P1<T, O extends keyof T> = O;
type A1<T extends ZU, A extends Exclude<keyof T, "type">> = P1<T, A>;
type A2<T extends ZT, A extends Exclude<keyof T["_zod"], "type">> = P1<T["_zod"], A>;
type A3<T extends ZT, A extends Exclude<keyof T["_zod"]["def"], "type">> = P1<T["_zod"]["def"], A>;
type A4<T extends ZT, A extends keyof T["_zod"]["def"]> = P1<T["_zod"]["def"], A>;
function f<T extends ZT, A extends Exclude<keyof T["_zod"]["def"], "type">>(a: A) { const k: keyof T["_zod"]["def"] = a; }
function g<T extends ZU, A extends Exclude<keyof T, "type">>(a: A) { const k: "checks" = a; const j: keyof T = a; }
