import { createRootRoute, HeadContent, Outlet, Scripts } from "@tanstack/react-router";

import favicon from "@/assets/favicon.svg?url&no-inline";
import { SiteProvider } from "@/components/site-provider";
import { UMAMI_SCRIPT } from "@/lib/analytics";
import { DEFAULT_LOCALE } from "@/lib/i18n";
import appCss from "@/styles/app.css?url";

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: "utf-8" },
      { name: "viewport", content: "width=device-width, initial-scale=1" },
      {
        title: "kardia",
      },
      {
        name: "description",
        content: "kardia：用 Rust 实现的 Cordis 插件框架，提供可撤销的副作用、响应式服务与热重载。",
      },
    ],
    links: [
      { rel: "stylesheet", href: appCss },
      {
        rel: "icon",
        type: "image/svg+xml",
        href: favicon,
      },
    ],
    scripts: [UMAMI_SCRIPT],
  }),
  component: RootComponent,
});

function RootComponent() {
  return (
    <html lang={DEFAULT_LOCALE} suppressHydrationWarning>
      <head>
        <HeadContent />
      </head>
      <body className="flex min-h-screen flex-col">
        <SiteProvider>
          <Outlet />
        </SiteProvider>
        <Scripts />
      </body>
    </html>
  );
}
