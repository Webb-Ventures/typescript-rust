// @target: es2016
// @module: esnext
// esbuild LowerAsyncSuperES2016NoBundle: super inside async methods and arrows, including
// optional calls and classes nested in async code.
declare class Base { [k: string]: any }
declare const objs: any[];
class Derived extends Base {
	async test(key: any) {
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
	a = super.a
	b = () => super.b
	c() { return super.c }
	d() { return () => super.d }
}
class Derived2 extends Base {
	async a() { return class { [super.foo] = 123 } }
	b = async () => class { [super.foo] = 123 }
}
for (let i = 0; i < 3; i++) {
	objs.push({
		__proto__: { foo() { return i } },
		async bar() { return super.foo() },
	})
}
