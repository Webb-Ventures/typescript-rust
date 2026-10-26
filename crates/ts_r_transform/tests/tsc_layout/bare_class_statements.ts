// @target: es2022
// @module: esnext
declare let g: <T>(...args: any) => any;
{ @g<number> class C {} }
(class D {});
<any>class E {};
<any>class F {}.name;
(<any>class G {});
(class H {} as any);
class I {} as any;
@g;
let x = 1;
