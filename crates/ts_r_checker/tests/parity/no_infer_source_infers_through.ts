declare const marker: unique symbol;
type Lazy<T> = (() => Promise<T>) & { [marker]: true };
type Rec = { [k: string]: number | Rec };
interface Router<Root, R extends Rec> { root: Root; record: R; make(): void }
type AnyRouter = Router<any, any>;
type Built<Root, R extends Rec> = Router<Root, R> & R;
type Options = { [k: string]: number | AnyRouter | Lazy<AnyRouter> };
type Decorate<T extends Options> = {
  [K in keyof T]: T[K] extends infer V
    ? V extends number ? V : V extends Router<any, infer R> ? R : V extends Lazy<Router<any, infer R>> ? R : never
    : never;
};
interface Builder<Root> { <T extends Options>(r: T): Built<Root, Decorate<T>> }
declare function init<Root>(): { router: Builder<Root> };
declare function lazy<T extends AnyRouter>(f: () => Promise<T | { [k: string]: T }>): Lazy<NoInfer<T>>;
const t = init<{ ctx: 1 }>();
const outer = t.router({ posts: lazy(async () => t.router({ list: 1 })) });
const n: string = outer.posts.list;
