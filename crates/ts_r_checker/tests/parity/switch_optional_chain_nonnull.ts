type Thing = { foo: string | number, bar(): number, baz: object };

function f40(o: Thing | undefined) {
    switch (o?.foo) {
        case "abc":
            o.foo;
            break;
        case 42:
            o.foo;
            break;
        case undefined:
            o.foo;
            break;
        default:
            o.foo;
            break;
    }
}

function f41(o: Thing | undefined) {
    switch (typeof o?.foo) {
        case "string":
            o.foo;
            break;
        case "number":
            o.foo;
            break;
        case "undefined":
            o.foo;
            break;
        default:
            o.foo;
            break;
    }
}

function grouped(o: Thing | undefined) {
    switch (o?.foo) {
        case "abc":
        case 42:
            o.foo;
            break;
        case "x":
        default:
            o.foo;
    }
}

type Shape =
    | { type: 'rectangle', width: number, height: number }
    | { type: 'circle', radius: number }

function getArea(shape?: Shape) {
    switch (shape?.type) {
        case 'circle':
            return Math.PI * shape.radius ** 2
        case 'rectangle':
            return shape.width * shape.height
        default:
            return 0
    }
}

type Test5 = {
  main?: {
    childs: Record<string, Test5>;
  };
};

function f50(obj: Test5) {
   for (const key in obj.main?.childs) {
      if (obj.main.childs[key] === obj) {
        return obj;
      }
   }
   return null;
}
export {};
