// trpc `httpBatchLink` (`resolveHTTPLinkOptions(opts)` with `opts:
// HTTPBatchLinkOptions<TRouter['_def']['_config']['$types']>` and a parameter of type
// `HTTPLinkBaseOptions<AnyClientTypes>`): an intersection with a deferred conditional member
// relates through its constraint (`{ url } & (Yes | No)`).
type Yes = { transformer: { a: 1 } };
type No = { transformer?: "err" };
type AnyClientTypes = { transformer: any; errorShape: any };
type TO<TRoot extends Pick<AnyClientTypes, "transformer">> = TRoot["transformer"] extends true
  ? Yes
  : No;
type Base<TRoot extends Pick<AnyClientTypes, "transformer">> = { url: string } & TO<TRoot>;
type Opts<TRoot extends AnyClientTypes> = Base<TRoot> & { max?: number };
declare function resolve(o: Base<AnyClientTypes>): void;
interface R<A> {
  _def: { _config: { $types: A } };
}
function f<TR extends R<any>>(
  opts: Opts<TR["_def"]["_config"]["$types"]>,
  o2: Base<TR["_def"]["_config"]["$types"]>,
  o3: TO<TR["_def"]["_config"]["$types"]>,
) {
  resolve(opts);
  resolve(o2);
  const x: TO<AnyClientTypes> = o3;
  const y: No = o3;
}
export {};
