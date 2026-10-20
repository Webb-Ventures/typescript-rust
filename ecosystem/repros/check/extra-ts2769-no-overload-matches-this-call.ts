// Cause:    extra TS2769: No overload matches this call.
// Found in: minimatch@10.2.6 src/ast.ts:130 (tests stage: its `tsc` build), a
//           `new Map<K, Map<K2, V>>([...])` whose inner `new Map([...])` has no type arguments
// Owner:    ts_r_checker (inference from the contextual type)
// Expected: tsc 7 infers the inner Map from the outer constructor's contextual type, so the
//           string literals stay literal (`Map<Kind | null, Kind | null>`). No error.
// Actual:   ts-r-check infers `Map<string, string>` for the inner Map and reports TS2769.
type Kind = '!' | '?' | '+' | '*' | '@';

export const usurp = new Map<Kind, Map<Kind | null, Kind | null>>([
  ['!', new Map([['!', '@']])],
  [
    '?',
    new Map([
      ['*', '*'],
      ['+', '*'],
    ]),
  ],
]);
