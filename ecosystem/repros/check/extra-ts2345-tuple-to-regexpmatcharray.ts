// Cause:    extra TS2345: Argument of type '…' is not assignable to parameter of type '…'.
// Found in: minimatch@10.2.6 src/index.ts:165 (tests stage: its `tsc` build),
//           `qmarksTestNoExt([$0])` where the parameter is `RegExpMatchArray`
// Owner:    ts_r_checker (assignability)
// Expected: tsc 7 accepts a `[string]` tuple for `RegExpMatchArray` (an `Array<string>` whose
//           extra members `index`, `input`, `groups` are all optional). No error.
// Actual:   ts-r-check reports TS2345 "Argument of type '[string]' is not assignable to
//           parameter of type 'RegExpMatchArray'."
const noExt = ([$0]: RegExpMatchArray) => $0.length;

export const test = ([$0, ext = '']: RegExpMatchArray) => noExt([$0]) + ext.length;
