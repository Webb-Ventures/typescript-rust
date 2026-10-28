// Ecosystem check repro (was ecosystem/repros/check/extra-ts2769-no-overload-matches-this-call.ts): extra TS2769: No overload matches this call.
// @filename: index.ts
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
