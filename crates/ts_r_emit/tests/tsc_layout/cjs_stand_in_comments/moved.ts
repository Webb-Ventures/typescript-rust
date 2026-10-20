class Foo {}
class Bar {}
export = Foo;
export { Bar }; // dropped with the erased export
