// Ecosystem check repro (was ecosystem/repros/check/extra-ts2344-type-does-not-satisfy-the-constraint.d.ts): extra TS2344: Type '…' does not satisfy the constraint '…'.
// @filename: index.d.ts
interface NoContext<FC extends undefined | void> {
  context?: FC;
}
export type FetchOptions<FC> = FC extends undefined | void ? NoContext<FC> : { context: FC };
