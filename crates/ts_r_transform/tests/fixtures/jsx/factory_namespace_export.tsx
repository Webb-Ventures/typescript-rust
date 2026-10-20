// @target: es2022
// @jsx: react
// The classic factory's root names an export of another block of the enclosing namespace:
// `y.React.createElement` (esbuild oracle: TSEnumJSX).
namespace x.y { export enum React { Fragment = 'div' } }
namespace x.y { console.log(<>test</>) }
