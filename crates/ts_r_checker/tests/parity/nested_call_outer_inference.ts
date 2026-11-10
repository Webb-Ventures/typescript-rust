// A call nested in a literal or function argument reads its contextual type through what
// the outer call inferred so far (tRPC's `testServerAndClientResource(router, { client: {
// links: [httpLink({ url })] } })` infers the router, not `AnyRouter`).
interface Router { _r: 1 }
interface Built<T> extends Router { t: T }
interface Op<R extends Router> { r: R; next(r: R): void }
type Link<R extends Router> = (rt: { x: 1 }) => Op<R>;
declare function link<R extends Router = Router>(o: { url: string }): Link<R>;
interface Client<R extends Router> { c: R }
interface Opts<R extends Router> {
  client?: Partial<{ links: Link<R>[] }> | ((o: { u: string }) => Partial<{ links: Link<R>[] }>);
}
declare function res<R extends Router>(r: R, o?: Opts<R>): { client: Client<R> };
declare const br: Built<string>;
export const n1: number = res(br, { client: { links: [link({ url: '' })] } }).client;
export const n2: number = res(br, { client(o) { return { links: [link({ url: o.u })] }; } }).client;
export const n3: number = res(br, { client: () => ({ links: [link({ url: '' })] }) }).client;
declare function arr<R extends Router>(r: R, l: Link<R>[]): Client<R>;
export const n4: number = arr(br, [link({ url: '' })]);

// A context whose type arguments come from an interface with a `this` type is no generic
// context: the call infers from it.
interface Builder<T> { create(): Built<{ ctx: T }>; me(): this }
declare const b: Builder<object>;
const built = b.create();
declare function one<R extends Router>(r: R, l: Link<R>): R;
export const n5: number = one(built, link({ url: '' }));

// Type parameters with nothing inferred yet read as what the outer call's contextual type
// implies (tRPC's `splitLink({ true: wsLink(..), false: httpBatchLink(..) })` in a
// `links` array), also when the literal has context-sensitive functions.
declare function split<R extends Router = Router>(o: { c: (x: number) => boolean; t: Link<R> | Link<R>[]; f: Link<R> }): Link<R>;
export const l1: Link<Built<string>> = split({ c: (x) => x > 1, t: link({ url: '' }), f: link({ url: '' }) });
export const l2: Link<Built<string>> = split({ c: (x: number) => x > 1, t: [link({ url: '' })], f: link({ url: '' }) });
export const n6: number = res(br, { client(o) { return { links: [split({ c(x) { return x > 1; }, t: link({ url: o.u }), f: link({ url: '' }) })] }; } }).client;
