declare module "typed-pkg" {
  interface Price {
    amount: number;
  }
  export interface Item {
    sku: string;
    price: Price;
  }
}
