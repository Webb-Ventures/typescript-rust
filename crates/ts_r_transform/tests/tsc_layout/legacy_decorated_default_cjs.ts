// @target: esnext
// @module: commonjs
// @experimentalDecorators: true
// @noEmitHelpers: true
declare var dec: any;
@dec
export default class {
    m() {}
}
