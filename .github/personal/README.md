# Windows 更新操作

在仓库的 Actions 中选择「同步作者并发布 Windows」，点击 Run workflow。

- upstream_tag 留空：使用作者最新正式 Release；也可以填写 v0.6.3 这样的正式版本。
- rebuild 默认不勾选：没有新版本就跳过；勾选后为当前正式版生成新的 sortfix 修订。
- 成功后从自己的 Releases 下载 setup.exe 或 portable.zip。
- Windows 测试与编译均在 GitHub 云端进行，不需要本地安装 Node、Rust 或依赖。
- 回复优先、Codex 默认排序及恢复会话去重修复作为独立功能补丁保留。
- 每次更新后 main 直接接在作者的正式版提交之后，只保留一个定制提交。
- 代码冲突、测试失败或编译失败时不更新 main、不发布新版。查看失败步骤及日志；
  不要使用 Discard commits 来解决问题。
- 发布阶段因网络失败时，优先在该次运行选择 Re-run failed jobs，可继续完成上传。

## 可选的每日自动检查

默认关闭。需要时在仓库 Settings → Secrets and variables → Actions → Variables
新建仓库变量 AUTO_UPDATE，值为 true。每天北京时间 11:23 检查作者最新正式版；
没有新正式版时不编译、不发版。把变量改为 false 即可关闭。手动按钮始终可用。
GitHub 的定时任务可能延迟，公开仓库长期无活动时也可能被自动停用。

## 维护功能

同步脚本从当前 main 与作者基线之间提取 src、src-tauri/src 和 test 的差异，
使用 Git 三方合并迁移功能。版本号及 Windows 配置由脚本单独生成；Actions
配置完整保留自己的版本。功能变更仍应先验证，并保持 main 只有一个定制提交。
