// A target signature no source overload matches is explained by the first source
// signature.
// A: first target fails against all
interface SA { (x: number): void; (x: boolean): void }
interface TA { (x: string): void; (x: number): void }
declare const sa: SA;
const ta: TA = sa;
// B: first target fails s0, passes s1; second target fails all
interface SB { (x: number): void; (x: string): void }
interface TB { (x: string): void; (x: boolean): void }
declare const sb: SB;
const tb: TB = sb;
// C: single source, several targets
interface SC { (x: number): void }
interface TC { (x: number): void; (x: string): void }
declare const sc: SC;
const tc: TC = sc;
// D: several sources, single target
interface SD { (x: number): void; (x: boolean): void }
interface TD { (x: string): void }
declare const sd: SD;
const td: TD = sd;
