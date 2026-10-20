// Cause:    extra TS2344: Type '…' does not satisfy the constraint '…'.
// Found in: lru-cache@11 dist/esm/index.d.ts (also glob, through lru-cache), zod, @types/node
// Owner:    ts_r_checker
// Expected: tsc 7 (strict, skipLibCheck false) reports nothing: in the true branch of
//           `FC extends undefined | void ? X : Y`, references to FC are narrowed to the
//           extends type, so `NoContext<FC>` satisfies `FC extends undefined | void`.
// Actual:   ts-r-check reports TS2344 "Type 'FC' does not satisfy the constraint 'void'."
// See also: extra-ts2344-record-property-key.d.ts, extra-ts2344-infer-in-template.d.ts
interface NoContext<FC extends undefined | void> {
  context?: FC;
}
export type FetchOptions<FC> = FC extends undefined | void ? NoContext<FC> : { context: FC };
