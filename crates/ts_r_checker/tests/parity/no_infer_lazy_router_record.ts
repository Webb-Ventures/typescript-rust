declare function lazy<T>(f: () => Promise<T>): () => Promise<NoInfer<T>>;
interface Router<TRoot, TRecord> { root: TRoot; record: TRecord }
interface Root<R = { ctx: object }> { router: <TIn>(_: TIn) => Router<R, TIn> }
declare const t: Root;
const l2 = lazy(async () => t.router({ list: 'x' }));
type Unlazy<T> = T extends () => Promise<Router<any, infer TRecord extends object>> ? TRecord : never;
const n: number = null! as Unlazy<typeof l2>;
