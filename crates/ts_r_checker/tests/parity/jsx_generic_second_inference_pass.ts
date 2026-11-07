// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"jsx":"preserve","noEmit":true,"lib":["es2015"],"types":[]},"files":["a.tsx"]}
// @filename: a.tsx
declare namespace JSX {
    export interface Element {}
    export interface IntrinsicAttributes {}
    export interface IntrinsicElements {}
}
interface BaseProps<T> {
  initialValues: T;
  nextValues: (cur: T) => T;
}
declare function Generic<Props = {}, Values = object>(p: Props & BaseProps<Values>): JSX.Element;
let a = <Generic initialValues={{ x: "y" }} nextValues={a => a} />;
let b = <Generic initialValues={12} nextValues={a => a} />;
let c = <Generic initialValues={{ x: "y" }} nextValues={a => ({ x: a.x })} />;
let d = <Generic initialValues={{ x: "y" }} nextValues={a => a.x} />;

interface ActionsObjectOr<State> {
    [prop: string]: ((state: State) => State) | State;
}
declare function App4<State, Actions extends ActionsObjectOr<State>>(props: Actions & { state: State }): JSX.Element;
const e = <App4 state={100} foo={s => s} />;
const f = <App4 state={100} foo={s => s.toFixed()} />;
