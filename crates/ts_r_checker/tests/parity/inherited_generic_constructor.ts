// An inherited constructor takes the extends clause's type arguments; keyof (X & T) reverse-maps T.
declare class Schema<T> { o: T }
type Target<E> = Schema<E> | string;
class Repo<E extends object> {
  constructor(target: Target<E>, m: number) {}
}
class Mongo<E extends object> extends Repo<E> {
  extra = 1;
}
export function get<Entity extends object>(t: Target<Entity>, t2: Schema<{ a: 1 }>): Repo<Entity> {
  const r = new Mongo(t, 1);
  const n: number = r;
  const r2 = new Mongo(t2, 1);
  const n2: number = r2;
  return r;
}
declare class Component<P> {
  constructor(props: Readonly<P>);
  readonly props: Readonly<P>;
}
class C<T> extends Component<{ x?: boolean } & T> {}
const y = new C({ foobar: "example" });
export const z: number = y.props.foobar;
