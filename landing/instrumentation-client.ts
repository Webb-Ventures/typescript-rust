// PostHog analytics, only in builds with NEXT_PUBLIC_POSTHOG_KEY set (CI maps it from the
// POSTHOG_KEY secret or variable; see AGENTS.md, Analytics). Local builds load nothing.
// posthog-js is imported lazily, after the page has loaded and the browser is idle, so it never
// competes with the page's own requests (it cost the home page's mobile LCP ~2 s otherwise).
const key = process.env.NEXT_PUBLIC_POSTHOG_KEY?.trim();
const DOCS = /^\/(docs|errors|benchmarks|search)(\/|$)/;

function start(key: string) {
  void import('posthog-js').then(({ default: posthog }) => {
    posthog.init(key, {
      api_host: process.env.NEXT_PUBLIC_POSTHOG_HOST || 'https://us.i.posthog.com',
      // Pageviews on history changes (client-side navigation), pageleave, autocapture.
      defaults: '2026-08-30',
      person_profiles: 'identified_only',
      // `site`: docs for the docs, error, benchmarks and search pages, landing for the rest,
      // decided per event so client-side navigation between them is counted right.
      before_send: event => {
        if (event) event.properties.site = DOCS.test(location.pathname) ? 'docs' : 'landing';
        return event;
      },
    });
    posthog.register({ site_env: process.env.NEXT_PUBLIC_POSTHOG_ENV || 'production' });
  });
}

if (key) {
  const idle = () =>
    'requestIdleCallback' in window ? requestIdleCallback(() => start(key), { timeout: 3000 }) : setTimeout(() => start(key), 1);
  if (document.readyState === 'complete') idle();
  else addEventListener('load', idle, { once: true });
}
