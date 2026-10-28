// @target: es2015
// @module: commonjs
export declare function f(): void;
export declare class C {}
export declare namespace N { const v: number; }
export declare enum E { A }
export declare var v: number;
f(); new C(); N.v; E.A; v = 2; v++; [v] = [1]; ({ v } = { v: 1 });
export declare let { d1, d2 }: any;
d1;
