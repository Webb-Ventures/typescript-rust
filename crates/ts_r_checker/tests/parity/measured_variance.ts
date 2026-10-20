// References to one generic interface or alias relate by measured variances: markers
// relate two instantiations; overloads of one instantiated type literal pair up
// (so Box<T> with an Opts<T> callback option is invariant).
interface Opts<T> { element?: (value: T) => T }
interface Box<T> {
  g: {
    <K>(key: (value: T) => K): K;
    <K>(key: (value: T) => K, options: Opts<T>): K;
  };
}
declare const a: Box<number>;
const b: Box<unknown> = a;

type Pair<T> = {
  (x: T): void;
  (x: T, o: Opts<T>): void;
};
declare const p: Pair<number>;
const q: Pair<unknown> = p;

type One<T> = { <K>(o: Opts<T>, k: K): void };
declare const o1: One<number>;
const o2: One<unknown> = o1;

interface Fn<T> { (x: T): void; (x: T, y: Opts<T>): void }
declare const f1: Fn<number>;
const f2: Fn<unknown> = f1;
const f3: Fn<1> = f1;
interface VA<T> { x: T }
interface VB<T> { f: (x: T) => void }
interface VC<T> { f: (x: T) => T }
declare const va: VA<number>; const va2: VA<string> = va;
declare const vb: VB<number>; const vb2: VB<string> = vb;
declare const vc: VC<number>; const vc2: VC<string> = vc;
