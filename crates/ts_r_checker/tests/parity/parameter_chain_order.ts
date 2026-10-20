// A parameter's pending property levels come below 'Types of parameters'.
interface O { element?: (value: number) => number }
interface P { element?: (value: 1) => 1 }
type F1 = (x: number, y: O) => void;
type F2 = (x: number, y: P) => void;
declare const f1: F1;
const f2: F2 = f1;
