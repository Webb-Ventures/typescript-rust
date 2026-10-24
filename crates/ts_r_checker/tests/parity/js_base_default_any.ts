// @filename: tsconfig.json
{"compilerOptions": {"allowJs": true, "strict": false, "target": "ES2023", "module": "commonjs", "noEmit": true}, "files": ["library.d.ts", "component.js", "typed_component.ts"]}
// @filename: library.d.ts
export class Foo<T = {}, U = {}, V = unknown, W = string> {
    props: T;
    state: U;
    v: V;
    w: W;
    constructor(props: T, state: U);
}
// @filename: component.js
import { Foo } from "./library";
export class MyFoo extends Foo {
    member;
}
// @filename: typed_component.ts
import { MyFoo } from "./component";
export class TypedFoo extends MyFoo {
    constructor() {
        super({x: "string", y: 42}, { value: undefined });
        this.props.x;
        this.state.value;
        this.v.anything;
        this.w.notThere;
        this.member;
    }
}
