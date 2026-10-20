declare function dec(...a: any[]): any;
@dec
export class D {
    @dec accessor y = 1;
    @dec static m() {}
}
export class P {
    accessor q = 1;
}
export const tag = (s: TemplateStringsArray) => s;
export const t = tag`x`;
export const meta = import.meta.url;
