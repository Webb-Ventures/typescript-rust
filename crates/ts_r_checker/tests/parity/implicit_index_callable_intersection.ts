// trpc `router.ts` (`isLazy(item)` on `AnyProcedure | AnyRouter | CreateRouterOptions |
// Lazy<AnyRouter>`): callable and constructable types get no implicit index signature, and
// neither does one member of a source intersection on its own.
interface Router { _def: { router: true } }
type Opts = { [key: string]: Router | Opts };
declare const f: () => Promise<unknown>;
const o1: Opts = f;
const o2: Record<string, unknown> = f;
declare const g: { (): void; x: number };
const o3: { [k: string]: number } = g;
declare const h: { new (): void };
const o4: { [k: number]: number } = h;
function fd() {}
const o5: Record<string, unknown> = fd;
declare const l2: (() => Promise<unknown>) & { [x: string]: true };
const o6: Opts = l2;
declare const l4: { a: Router } & { [x: string]: true };
const o7: Opts = l4;
declare const l5: { a: Router } & { b: Router };
const o8: Opts = l5;
const o9: Record<string, any> = f;
const lazyMarker = 'lazyMarker' as 'lazyMarker' & { __brand: 'lazyMarker' };
type Lazy<TAny> = (() => Promise<TAny>) & { [lazyMarker]: true };
type Opts2 = { [key: string]: Router | Opts2 | Lazy<Router> };
declare function isLazy<TAny>(input: unknown): input is Lazy<TAny>;
declare const i3: Opts2 | Lazy<Router>;
if (isLazy(i3)) {
  const n: null = i3;
}
// date-fns `MatchFn<number, Record<string, unknown>>` to `MatchFn<number, { unit: U }>`.
interface MatchOpts { width?: string }
type MatchFn<R, Extra = Record<string, unknown>> = (s: string, o?: MatchOpts & Extra) => R | null;
declare const mf: MatchFn<number>;
const mf2: MatchFn<number, { unit: "a" | "b" }> = mf;
