// @target: es2016
// @module: esnext
// esbuild LowerStaticAsyncSuperES2016NoBundle: super in static async methods and static
// initializers of classes nested in async code.
declare class Base { [k: string]: any }
class Derived extends Base {
	static async test(key: any) {
		return [
			await super.foo,
			await super[key],
			await ([super.foo] = [0]),
			await ([super[key]] = [0]),
			await (super.foo = 1),
			await (super[key] = 1),
			await (super.foo += 2),
			await (super[key] += 2),
			await ++super.foo,
			await ++super[key],
			await super.foo++,
			await super[key]++,
			await super.foo.name,
			await super[key].name,
			await super.foo?.name,
			await super[key]?.name,
			await super.foo(1, 2),
			await super[key](1, 2),
			await super.foo?.(1, 2),
			await super[key]?.(1, 2),
			await (() => super.foo)(),
			await (() => super[key])(),
			await (() => super.foo())(),
			await (() => super[key]())(),
			await super.foo``,
			await super[key]``,
		]
	}
}
let fn = async () => class extends Base {
	static a = super.a
	static b = () => super.b
	static c() { return super.c }
	static d() { return () => super.d }
}
class Derived2 extends Base {
	static b = async () => super.foo
}
