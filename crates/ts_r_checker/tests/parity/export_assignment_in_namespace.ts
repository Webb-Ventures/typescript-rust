// Export assignments directly in a namespace are TS1063/TS1319; the expression is not checked.
namespace M {
    export = A;
}
namespace N {
    export default foo;
}
declare namespace D {
    export = B;
}
