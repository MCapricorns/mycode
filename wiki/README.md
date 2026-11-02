# Wiki 维护说明

[在线 Wiki](https://mcapricorns.github.io/mycode/wiki/index.html) 提供模块图、运行时说明、CodeMode 协议和源码阅读。页面把图谱、两篇架构文档及相关源码内嵌到一个 HTML 文件，下载仓库后也能直接打开 wiki/index.html 离线阅读。

## 文档分工

| 文件 | 内容 |
| --- | --- |
| [README](../README.md) | 安装、使用、配置、贡献者和开发入口 |
| [运行时架构](architecture/runtime.md) | 两个 package、内部模块、回合、会话、子代理与取消 |
| [CodeMode](architecture/code-mode-engine.md) | 同步脚本、工具返回、MCP、资源预算与收集协议 |
| [性能与资源记录](memory-report.md) | 体积测量方法、性能证据和局部资源上限 |
| graph.json | 图谱节点、关系、源码路径和稳定深链接 ID |

运行时 package 是 mycode，GPUI package 是 mycode-desktop。图谱中的 frontend、app、agent、tools、providers、config、core 是 mycode 内部模块。不要把模块重新写成独立 Cargo 依赖。

## 生成与检查

仅需 Python 标准库，在仓库根目录执行：

```sh
python scripts/build_wiki.py
python scripts/build_wiki.py --check
python -B -m unittest discover -s wiki/tests -v
```

先更新文档和 graph.json，再运行生成器。源码有修改时也要重新生成，因为离线页面嵌入的是源码快照。不要手改 wiki/index.html 或生成的品牌素材。

生成器验证节点 ID、关系、分栏和源码路径；测试还检查离线章节链接、HTML 转义、脚本数据封装和品牌来源。页面能通过 --check 表示快照与当前输入一致，不等于 Rust 代码或 UI 已通过测试。

## 页面来源

scripts/build_wiki.py 渲染受限 Markdown 并注入 scripts/wiki_shell.html。支持标题、段落、行内代码、代码块、链接、加粗、单层列表和表格。旧标题的独立空锚点用于深链接兼容，其他原始 HTML 会转义。

品牌标识来自 crates/mycode-desktop/assets/icon.svg。生成器同步 wiki/images/logo.svg 和 banner.svg；桌面 ICO、PNG、ICNS 由单独的图标脚本维护。

仓库根 index.html 跳转到 Wiki，.nojekyll 让 Pages 直接发布静态文件。现有发布来源为 main 分支根目录。

## 导航与兼容

节点链接使用 #n=<id>，源码链接可附加 &f=<path>；文档使用 #v=architecture 或 #v=code-mode，章节附加 &s=<heading-id>。模块改名后保持节点 ID，并更新源码路径。旧 package 源码路径不再存在，新的链接应指向 crates/mycode/src/。

页面支持搜索、键盘聚焦、Enter/Space 打开节点、Escape 关闭源码窗口及深浅主题。小屏幕使用节点列表与详情视图。

文档以实现和测试为依据。报告验证时列出实际执行的命令与平台，不把测试文件的存在、旧截图或历史基准当作本次验证。
