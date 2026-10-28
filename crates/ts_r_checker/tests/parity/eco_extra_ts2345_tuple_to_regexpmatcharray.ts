// Ecosystem check repro (was ecosystem/repros/check/extra-ts2345-tuple-to-regexpmatcharray.ts): extra TS2345: Argument of type '…' is not assignable to parameter of type '…'.
// @filename: index.ts
const noExt = ([$0]: RegExpMatchArray) => $0.length;

export const test = ([$0, ext = '']: RegExpMatchArray) => noExt([$0]) + ext.length;
