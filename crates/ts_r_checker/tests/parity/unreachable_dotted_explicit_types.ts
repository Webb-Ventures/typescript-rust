// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"noEmit":true,"target":"ES2022","allowUnreachableCode":false},"files":["a.ts"]}
// @filename: a.ts
function inferredRoot() {
    const obj = { fail(): never { throw 0; } };
    obj.fail();
    1;
}
function annotatedRoot(obj: { fail(): never }) {
    obj.fail();
    1;
}
class Calls {
    inferred = { fail(): never { throw 0; } };
    annotated: { fail(): never } = { fail() { throw 0; } };
    property: () => never = () => { throw 0; };
    inferredProperty = (): never => { throw 0; };
    inferredMiddle() {
        this.inferred.fail();
        1;
    }
    annotatedMiddle() {
        this.annotated.fail();
        1;
    }
    annotatedProperty() {
        this.property();
        1;
    }
    unannotatedProperty() {
        this.inferredProperty();
        1;
    }
}
function typedInstance(obj: Calls) {
    obj.inferred.fail();
    1;
}
function typedInstanceAnnotated(obj: Calls) {
    obj.annotated.fail();
    1;
}
function annotatedFunctionProperty(obj: { fail: () => never }) {
    obj.fail();
    1;
}
function inferredAssertionRoot(value: unknown) {
    const obj = { assert(value: unknown): asserts value is string { if (typeof value !== "string") throw 0; } };
    // @ts-ignore
    obj.assert(value);
    const s: string = value;
}
function annotatedAssertionRoot(obj: { assert(value: unknown): asserts value is string }, value: unknown) {
    obj.assert(value);
    const s: string = value;
}
