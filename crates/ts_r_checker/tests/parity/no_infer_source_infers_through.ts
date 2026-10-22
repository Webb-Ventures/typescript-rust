interface Box<T> { value: T }
interface Root<R = { ctx: string }> { box: Box<R> }
type Unbox<T> = T extends Promise<Box<infer V>> ? V : never;
const n: number = null! as Unbox<Promise<NoInfer<Root['box']>>>;
