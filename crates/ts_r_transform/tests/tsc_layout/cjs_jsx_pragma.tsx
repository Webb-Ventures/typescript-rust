// @module: commonjs
// @target: es2022
// @jsx: react
/** @jsx dom */
// A one-name imported factory is called without its module as `this`.
import { dom } from "./renderer";
export const el = <div>{1}{2}</div>;
