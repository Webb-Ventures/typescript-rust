interface R<C> { _def: { ctx: C } }
type AnyR = R<any>;
interface Base<T> { router: T }
type CtxD<T extends AnyR> = T['_def']['ctx'] extends object ? { a?: 1 } : { a: 1 };
export function f1<T extends AnyR>(o: Partial<Base<T> & CtxD<T>>, t: T) {
  o.router.toString();
  const s: { router: T } = { router: t, ...o };
}
