// A switch case of type null or undefined itself is never TS2678; a union of them is.
declare const s: string;
declare const n: 0 | 1;
declare const o: { a: number };
switch (s) { case null: break; case undefined: break; }
switch (n) { case null: break; case undefined: break; }
switch (o) { case null: break; case undefined: break; }
switch (null) { case s: break; }
switch (undefined) { case n: break; }
declare const u: undefined;
switch (s) { case u: break; }
declare const nl: null | undefined;
switch (s) { case nl: break; }
if (s === null) {}
if (s === u) {}
