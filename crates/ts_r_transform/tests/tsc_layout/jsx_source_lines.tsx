// @target: es2015
// @module: esnext
// @jsx: react
declare var React: any;
declare var ok: boolean;
export const list = [
    <meta />,
    <meta />
];
export const inline = [<b />, <i />];
export const pick = ok
    ? <span />
    : <div>{ok}</div>;
export const call = f(
    <a />,
    <b />
);
declare function f(...a: any[]): any;
