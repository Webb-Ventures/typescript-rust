// An array-like type with a literal `length` is a tuple context for an array literal.
type EmptyTuple = [];
interface MyEmptyTuple extends EmptyTuple { extraInfo?: any; }
const withExtra: MyEmptyTuple = [];
interface Pair extends Array<number> { length: 2 }
const p: Pair = [1, 2];
const p3: Pair = [1, 2, 3];
const z: undefined = [] as MyEmptyTuple;
