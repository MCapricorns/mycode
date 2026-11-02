# 怎样阅读和更新 Wiki

[在线 Wiki](https://mcapricorns.github.io/mycode/wiki/index.html) 可以查看模块关系、开发说明和源码。下载仓库后，直接打开 wiki/index.html 也能阅读：页面已经包含两篇说明和图中列出的源码，不需要启动服务器。

## 去哪里找信息

| 想了解什么 | 看这里 |
| --- | --- |
| 安装、模型连接、日常操作、编译测试 | [项目 README](../README.md) |
| 发消息后代码如何运行，数据存在哪里 | [MYCode 如何运行](architecture/runtime.md) |
| 批量调用工具、async/await、并发、错误和停止 | [CodeMode 使用说明](architecture/code-mode-engine.md) |
| 已测得的耗时、程序体积和资源限制 | [性能记录](memory-report.md) |
| 每个模块的说明、源码入口和关联 | 在线页面的模块图；数据来自 graph.json |

项目只有 mycode 和 mycode-desktop 两个 Cargo package。frontend、app、agent、tools、providers、config、core 都在 mycode 内部。

## 改完内容后怎样生成页面

在仓库根目录执行，Python 标准库即可：

```sh
python scripts/build_wiki.py
python scripts/build_wiki.py --check
python -B -m unittest discover -s wiki/tests -v
```

先修改 Markdown、graph.json 或源码，再生成页面。源码改动也要重新生成，因为页面内嵌的是生成时的文件内容。提交时一起提交 wiki/index.html；不要直接编辑它。

--check 会检查页面是否与当前输入一致。测试会检查链接、HTML 转义、内嵌源码和图片来源。Rust 和桌面测试仍按 README 中的命令单独运行。

## 哪个文件控制页面

- scripts/build_wiki.py：读取文档和源码，生成离线页面。
- scripts/wiki_shell.html：页面布局、导航、搜索和源码窗口。
- wiki/graph.json：模块名称、说明、关联和源码路径。
- wiki/architecture/：在线页面中的两篇说明。
- crates/mycode-desktop/assets/icon.svg：品牌图标原稿，生成器据此更新 Wiki 的 logo.svg 和 banner.svg。

Markdown 支持标题、段落、行内代码、代码块、链接、加粗、单层列表和表格。已有的独立空锚点用于保留旧链接，其余原始 HTML 会显示为文字。

根目录 index.html 会跳转到 Wiki。GitHub Pages 从 main 分支根目录发布这些静态文件。

## 保留旧链接

模块链接采用 #n=<id>，源码可以再加 &f=<path>。两篇说明分别是 #v=architecture 和 #v=code-mode，章节再加 &s=<heading-id>。

修改模块名称时保留节点 id；移动源码时更新 files 中的路径。改写章节标题时保留原有空锚点，这样别人保存的链接仍然能打开。

文案先说明这个功能做什么，再说明怎么使用或去哪里改代码。命令、限制和行为以当前实现为准；历史性能结果标明版本和测量条件。
