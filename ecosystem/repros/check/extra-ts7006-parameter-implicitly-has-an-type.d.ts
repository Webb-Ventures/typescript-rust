// Cause:    extra TS7006: Parameter '…' implicitly has an '…' type.
// Found in: ajv@8.20.0 dist/compile/codegen/index.d.ts (`private set _currNode(value);`)
// Owner:    ts_r_checker
// Expected: tsc 7 (strict) reports nothing: declaration emit drops the types of private
//           members, and noImplicitAny is not reported for them in a .d.ts.
// Actual:   ts-r-check reports TS7006 "Parameter 'value' implicitly has an 'any' type."
export declare class CodeGen {
  private get _root();
  private set _currNode(value);
}
