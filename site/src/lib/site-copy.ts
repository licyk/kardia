import { useI18n } from "fumadocs-ui/contexts/i18n";

import { DEFAULT_LOCALE, isLocale, type Locale } from "@/lib/i18n";

const zhCN = {
  pageRailLabel: "页面区块导航",
  theme: {
    hint: "点击切换主题",
    modes: { system: "主题：跟随系统", light: "主题：浅色", dark: "主题：深色" },
  },
  docs: {
    comments: "评论",
  },
  home: {
    sections: ["首页", "特性", "流程", "亮点", "安装"],
    hero: {
      eyebrow: "Cordis 的 Rust 实现",
      title: "能干净撤销的插件，",
      titleAccent: "随时组合，随时替换",
      description:
        "kardia 是一个插件框架：插件做过的每件事都能被完整撤销，服务在依赖变化时自动响应，配置文件的修改即刻作用于运行中的应用，访客插件可以原地热重载。它用 Rust 实现了论文《A Programming Paradigm for Spatiotemporal Composability》和 TypeScript 框架 Cordis。",
      start: "开始使用",
      learn: "了解",
      platformsLabel: "支持的平台",
    },
    features: {
      title: "组合插件所需的一切",
      description: "每项特性都有对应的指南，示例代码可以直接运行。",
      open: "阅读指南",
      items: [
        {
          title: "可撤销效应",
          description: "监听器、定时器、服务、子插件都被追踪；插件停止时，它做过的一切按顺序撤销。",
          href: "/docs/guide-effects",
        },
        {
          title: "响应式服务",
          description: "插件声明依赖的服务：依赖齐全时启动，被替换时重启，消失时停止。",
          href: "/docs/guide-services",
        },
        {
          title: "声明式配置",
          description: "用 YAML 或 JSON 描述整个应用；编辑文件即可重新配置运行中的应用，运行时的修改也会写回文件。",
          href: "/docs/guide-configuration",
        },
        {
          title: "热重载",
          description: "监视模块文件，按批次原地替换插件，失败的部分自动回滚，其余照常生效。",
          href: "/docs/guide-hot-reload",
        },
        {
          title: "访客插件",
          description: "用 WASM、动态库或独立进程运行插件；同一份 Rust 代码可以编译成三种形式。",
          href: "/docs/guide-guests",
        },
      ],
    },
    workflow: {
      title: "从安装到热重载",
      description: "四步搭起一个可以在运行中修改的应用。",
      steps: [
        {
          title: "安装",
          description: "用 cargo add kardia 加入依赖，或用 cargo install kardia 安装命令行工具。",
        },
        {
          title: "编写插件",
          description: "用 #[plugin] 声明依赖的服务；在插件里注册的一切都会在它停止时自动撤销。",
        },
        {
          title: "描述应用",
          description: "在 kardia.yml 里列出插件、分组、隔离与配置，内置插件和访客插件都可以直接引用。",
        },
        {
          title: "运行中修改",
          description: "编辑配置或访客插件，kardia 只重启受影响的插件，其余继续运行。",
        },
      ],
    },
    highlights: {
      title: "忠实移植，经过验证",
      description: "kardia 按 Cordis 的代码与论文的定理实现，并用测试逐条核对。",
      items: [
        {
          title: "移植 Cordis 的全部测试",
          description: "TypeScript 实现的每个测试用例都已移植，或注明了不适用于 Rust 的原因。",
        },
        {
          title: "用性质测试检验定理",
          description: "随机生成插件编排与调度，检查论文的保持性、恢复精确性、顺序与汇合定理。",
        },
        {
          title: "编译期检查依赖",
          description: "#[plugin] 从签名读出依赖，访问未声明的服务无法通过编译。",
        },
        {
          title: "单锁并发模型",
          description: "每个根上下文只有一把锁，用户代码从不在锁内运行，状态转换可以安全地并发进行。",
        },
        {
          title: "三种访客后端",
          description: "WASM 沙箱、开发用的动态库、独立的子进程，共用同一套访客接口。",
        },
      ],
    },
    install: {
      title: "几分钟内开始",
      description:
        "kardia 发布在 crates.io。安装文档介绍所需的 Rust 版本、依赖写法、命令行工具，以及构建 WASM 访客插件所需的目标平台。",
      link: "查看安装文档",
    },
    footer: {
      label: "页脚导航",
      docs: "文档",
      quickStart: "快速开始",
      guide: "使用指南",
      repository: "源代码",
      socialLabel: "社交链接",
      social: { blog: "博客", github: "GitHub", bilibili: "哔哩哔哩" },
    },
    preview: {
      label: "kardia 插件树运行示意",
      pause: "暂停演示",
      play: "播放演示",
      workspace: "插件树",
      select: "kardia.yml",
      create: "添加条目",
      search: "搜索插件",
      plugins: [
        {
          detail: "提供 database",
          status: "提供中",
          operationTitle: "提供 database 服务",
          operationDetail: "postgres://primary",
        },
        {
          detail: "依赖 database、timer",
          status: "运行中",
          operationTitle: "已提交 database 视图",
          operationDetail: "timer.interval 每 1 秒",
        },
        {
          detail: "WASM 访客插件",
          status: "热重载中",
          operationTitle: "重新加载 plugins/hello.wat",
          operationDetail: "旧实例已释放 · 新实例已启动",
        },
      ],
    },
  },
} as const;

type TranslationShape<T> = T extends string
  ? string
  : T extends readonly unknown[]
    ? { readonly [K in keyof T]: TranslationShape<T[K]> }
    : T extends object
      ? { readonly [K in keyof T]: TranslationShape<T[K]> }
      : T;

export type SiteCopy = TranslationShape<typeof zhCN>;

const en = {
  pageRailLabel: "Page sections",
  theme: {
    hint: "Click to switch theme",
    modes: { system: "Theme: follow system", light: "Theme: light", dark: "Theme: dark" },
  },
  docs: {
    comments: "Comments",
  },
  home: {
    sections: ["Home", "Features", "Workflow", "Highlights", "Install"],
    hero: {
      eyebrow: "Cordis, implemented in Rust",
      title: "Plugins that clean up",
      titleAccent: "after themselves",
      description:
        "kardia is a plugin framework in which everything a plugin does can be reverted, services follow their dependencies as they change, config file edits apply to the running app, and guest plugins reload in place. It implements the paper “A Programming Paradigm for Spatiotemporal Composability” and its TypeScript framework, Cordis, in Rust.",
      start: "Get started",
      learn: "Explore ",
      platformsLabel: "Supported platforms",
    },
    features: {
      title: "What composing plugins takes",
      description: "Every feature has its own guide, with examples you can run.",
      open: "Read the guide",
      items: [
        {
          title: "Revertible effects",
          description:
            "Listeners, timers, services and child plugins are tracked; when a plugin stops, everything it did is undone in order.",
          href: "/docs/guide-effects",
        },
        {
          title: "Reactive services",
          description:
            "Plugins declare the services they use: they start when those exist, restart when one is replaced, and stop when one goes away.",
          href: "/docs/guide-services",
        },
        {
          title: "Declarative configuration",
          description:
            "Describe the app in YAML or JSON; edit the file to reconfigure the running app, and runtime changes are written back.",
          href: "/docs/guide-configuration",
        },
        {
          title: "Hot reload",
          description:
            "Watch module files and replace plugins in place, batch by batch; failed parts roll back while the rest applies.",
          href: "/docs/guide-hot-reload",
        },
        {
          title: "Guest plugins",
          description:
            "Run plugins as WASM modules, dynamic libraries or child processes; one Rust source builds all three.",
          href: "/docs/guide-guests",
        },
      ],
    },
    workflow: {
      title: "From install to hot reload",
      description: "Four steps to an app you can change while it runs.",
      steps: [
        {
          title: "Install",
          description: "Add the dependency with cargo add kardia, or the command-line tool with cargo install kardia.",
        },
        {
          title: "Write a plugin",
          description:
            "Declare the services it uses with #[plugin]; everything it registers is undone when it stops.",
        },
        {
          title: "Describe the app",
          description:
            "List plugins, groups, isolation and config in kardia.yml, referring to built-in and guest plugins alike.",
        },
        {
          title: "Change it while it runs",
          description: "Edit the config or a guest plugin; kardia restarts only what is affected and keeps the rest running.",
        },
      ],
    },
    highlights: {
      title: "A faithful port, verified",
      description: "kardia follows Cordis's code and the paper's theorems, and its tests check them point by point.",
      items: [
        {
          title: "The Cordis test suite, ported",
          description:
            "Every test case of the TypeScript implementation is ported, or recorded with why it does not apply to Rust.",
        },
        {
          title: "Theorems as property tests",
          description:
            "Random plugin orchestrations and schedules check the paper's preservation, recovery, ordering and confluence theorems.",
        },
        {
          title: "Dependencies checked at compile time",
          description: "#[plugin] reads a plugin's dependencies from its signature; reaching an undeclared service does not compile.",
        },
        {
          title: "One lock, no user code under it",
          description:
            "Each root context has a single lock that user code never runs under, so transitions run safely in parallel.",
        },
        {
          title: "Three guest backends",
          description: "A WASM sandbox, dynamic libraries for development and child processes share one guest interface.",
        },
      ],
    },
    install: {
      title: "Start in minutes",
      description:
        "kardia is published on crates.io. The installation guide covers the Rust version, the dependency, the command-line tool, and the target needed to build WASM guest plugins.",
      link: "Read the installation guide",
    },
    footer: {
      label: "Footer",
      docs: "Docs",
      quickStart: "Quick start",
      guide: "Guide",
      repository: "Source code",
      socialLabel: "Social links",
      social: { blog: "Blog", github: "GitHub", bilibili: "bilibili" },
    },
    preview: {
      label: "Sketch of a running kardia plugin tree",
      pause: "Pause the demo",
      play: "Play the demo",
      workspace: "Plugin tree",
      select: "kardia.yml",
      create: "Add entry",
      search: "Search plugins",
      plugins: [
        {
          detail: "provides database",
          status: "Providing",
          operationTitle: "Providing the database service",
          operationDetail: "postgres://primary",
        },
        {
          detail: "uses database, timer",
          status: "Active",
          operationTitle: "Committed to database",
          operationDetail: "timer.interval every 1 s",
        },
        {
          detail: "WASM guest plugin",
          status: "Reloading",
          operationTitle: "Reloading plugins/hello.wat",
          operationDetail: "old instance disposed · new one applied",
        },
      ],
    },
  },
} as const satisfies SiteCopy;

const SITE_COPY: Record<Locale, SiteCopy> = { "zh-CN": zhCN, en };

export function getSiteCopy(locale: Locale): SiteCopy {
  return SITE_COPY[locale];
}

export function useSiteLocale(): Locale {
  const { locale } = useI18n();
  return isLocale(locale) ? locale : DEFAULT_LOCALE;
}

export function useSiteCopy(): SiteCopy {
  return getSiteCopy(useSiteLocale());
}
