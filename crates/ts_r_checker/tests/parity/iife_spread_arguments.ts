// Parameters of an immediately invoked function called with spread arguments: a spread
// tuple fills one parameter per element, an array or `any` spread one parameter of its
// element type; a rest parameter takes the rest as a tuple (rxjs's zip-legacy specs).
declare const values: any;
((r1, r2) => { const a: boolean = r1; const b: boolean = r2; })(...values);
declare const arr: number[];
((r1, r2) => { const a: boolean = r1; const b: boolean = r2; })(...arr);
declare const tup: [number, string];
((r1, r2) => { const a: boolean = r1; const b: boolean = r2; })(...tup);
((r0, r1, r2) => { const a: boolean = r1; const b: boolean = r2; })(1, ...tup);
((r1, r2, r3) => { const a: boolean = r1; const b: boolean = r2; const c: boolean = r3; })(1, ...arr);
((...r) => { const a: boolean = r; })(...arr);
((a, ...r) => { const b: boolean = r; })(1, ...arr);
((a, ...r) => { const c: boolean = r; })(true, 's', ...arr);
