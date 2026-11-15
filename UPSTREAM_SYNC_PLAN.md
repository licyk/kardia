# Cordis 上游同步执行计划

对照基线：`f6290db` → `36dc7fb5e271143039e6b8b6df59840fc0886c1b`。
另修复已确认的 Rust 重复 disposal 提前完成问题；上游对应分支尚未合并。

每阶段验证后提交推送并更新本文；完成后将结果写入双语文档网站并删除本文。不发布版本、不新增 JS FS/Server 包，不改变手动触发的上游对照工作流。

- [x] 路由保留路径中的 `+`；Proxy 按段编码、限制目标目录、正确处理 target 查询和片段；覆盖 HTTP/WS 实际请求。
- [x] Rust effect 重复 disposal 等待同一次清理并共享结果；保留同步清理和异步任务不因句柄丢弃而取消的语义。
- [ ] FS base/intercept 统一为目录 URL，移除查询/片段，拒绝 opaque base；Windows 盘符入口转换；Static、include、HMR、CLI 模块解析使用作用域 FS 目录。
- [ ] 更新固定上游基线、支持参数化用例的清单生成器、真实对照与回归测试。
- [ ] 更新双语网站说明，完成架构/格式/相关测试和原生 CI 验证，删除计划书。

强制验收：故障隔离始终有效；手动（默认）、限速自动重试、宿主自定义三种恢复策略均保留；清理完成后才替换实例，显式销毁取消恢复，无关插件持续运行。Linux 本地结果与 Windows/macOS 原生 CI 分开记录。

## 验证记录

阶段 1：`cargo test -p kardia-server -p kardia-server-proxy` 通过，含真实 HTTP/WS 编码与目录限制、三种恢复策略持续服务测试。新的上游联合对照在阶段 4 更新基线后运行。

阶段 2：`cargo test -p kardia-core` 通过，覆盖重复等待、同步/异步 setup、共享清理错误、丢弃首个句柄后继续清理，以及现有 recovery 故障注入测试。
