// An instantiated indexed access whose key is missing from a plain interface is `unknown`
// (Next.js `PageProps<'/route'>` with a route the generated map lacks; private-app
// apps/web).
interface ParamMap { "/": {} }
type AppRoutes = "/"
interface PageProps<AppRoute extends AppRoutes> {
  params: Promise<ParamMap[AppRoute]>
  direct: ParamMap[AppRoute]
}
export async function Page({ params, direct }: PageProps<'/x/[id]'>) {
  const { id } = await params
  const n = (await params).short
  direct.foo
  return [id, n]
}
