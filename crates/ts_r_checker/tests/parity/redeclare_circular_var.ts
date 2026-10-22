// A `var` whose initializer refers to itself is `any` (TS7022), and a later declaration
// with another type is TS2403 against `any`; one declared `any` agrees.
var co1 = (co1, 3);
var co1: number;
var as1 = (as1 = 2);
var as1: number;
var cnd1 = cnd1 ? 0 : 1;
var cnd1: number;
var and1 = and1 && "";
var and1: string;
var propAcc1 = { m: propAcc1.m };
var propAcc1: { m: any; };
const c1 = (c1, 3);
export {};
var or1 = or1 || "";
var or1: any;
