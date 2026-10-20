// `typeof x.p` where `x` is `any` is `any`, also inside indexed access and unions.
declare const offers: any
const k: keyof typeof offers.packs = true
const e: (typeof offers.packs)[number] = 1
type U = (typeof offers.packs | typeof offers.plans)[number]
const u: keyof U = false
export {}
