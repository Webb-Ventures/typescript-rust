// Computed keys of binding patterns: untyped parameters read the type their pattern
// implies (`{}`); a key that is not a key type is TS2464 (and TS2538 unless the parent is any).
declare const o: object;
declare const k: any;
declare const s: string;
declare const n: number;
function g({[k]: x}) {}
function g2({[s]: x}) {}
function g3({[n]: x}) {}
class C { m({[k]: x}) {} m2({[s]: x}) {} }
(function ({[k]: x}) {})({});
(function ({[s]: x}) {})({});
const o = { m({[k]: x}) {}, m2({[s]: x}) {} };
const a = ({[k]: x}) => {};
const a2 = ({[s]: x}) => {};
function g4({[k]: x}, {[s]: y}) {}
declare const b: boolean;
const {[o]: z}: any = {};
const {[b]: z2}: { [k: string]: number } = {};
function f<T>(t: T) { const {[o]: q} = t; }
const {[o]: z3, ...rest}: any = {};
(({ [s]: x = "" }) => {})();
(({ [s]: x }) => {})();
(function ({ [s]: x = "" }) {})();
