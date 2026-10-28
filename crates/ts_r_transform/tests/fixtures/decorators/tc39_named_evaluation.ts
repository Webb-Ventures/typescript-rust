// @target: es2022
declare const dec: any;

const obj = { a: @dec class {}, "b c": @dec class {}, 1: @dec class {}, ["d"]: @dec class {} };
const { e = @dec class {}, f: g = @dec class {} } = obj as any;
let h: any;
h = @dec class {};
class Holder {
    static field = @dec class {};
    member = @dec class {};
}
export default (@dec class {});
