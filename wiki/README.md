
<a id="mycode-wiki"></a>

# MYCode 知识库

打开[在线知识库](https://mcapricorns.github.io/mycode/wiki/index.html)，可浏览架构图、运行时说明和 CodeMode 约定。GitHub Pages 已从 `main` 分支根目录发布静态文件；根 `index.html` 跳转到知识库，`.nojekyll` 跳过 Jekyll 处理。已有静态快照不需要额外的部署流程。

生成页面内嵌图谱、两篇架构文档及图谱引用的源码。下载仓库后，用浏览器打开 `wiki/index.html`，即可离线导航、搜索和阅读这些源码；内嵌内容反映生成时的工作区，不能替代当前实现。

## 阅读顺序

- [运行时架构](architecture/runtime.md)：先看消息、子代理、固定流程、人工整合和显式返工，再看包边界、账本与投影。
- [代码编排引擎](architecture/code-mode-engine.md)：同步 `call`、提交与收集、错误和图片、MCP、资源上限及取消。
- [内存占用研究报告](memory-report.md)：已复核的局部上限与旧基准的证据边界；这篇是仓库文档，不在当前页面的两篇内嵌架构正文中。

<a id="source-of-truth"></a>

## 文档与生成来源

实现和对应测试是行为依据，知识库负责解释实际约定。审计应分别确认“源码有此行为”“回归覆盖此行为”和“本次确实执行了检查”，避免把历史记录当作当前验证结果。

| 路径 | 用途 |
| --- | --- |
| `wiki/graph.json` | 图谱分栏、节点、关系和内嵌源码路径；稳定标识用于深链接 |
| `wiki/architecture/runtime.md` | 使用流程、包边界、回合、子代理、账本和投影 |
| `wiki/architecture/code-mode-engine.md` | MYCode 内置 CodeMode 与远端工具约定 |
| `wiki/memory-report.md` | 内存审计说明及明确标记的历史基准 |
| `scripts/wiki_shell.html` | 页面导航、图谱、搜索、源码阅读与主题 |
| `scripts/build_wiki.py` | 渲染两篇架构文档，将图谱和源码嵌入 `wiki/index.html`，并从桌面 SVG 生成知识库品牌素材 |

修改文档或图谱输入后重新生成页面；不手工编辑 `wiki/index.html`。源码链接指向 GitHub，两篇架构文档之间的链接保留在离线页面内。生成器使用受限 Markdown，支持标题、段落、行内代码、加粗、链接、代码块、单层无序列表和表格。改名标题前的独立空锚点行用于旧章节链接兼容；其他原始 HTML 仍应转义。

知识库与桌面共享纸色和墨色：`#FAF6EE`、`#F3EDE2`、`#14131B`、`#C72E38`；暗色主题使用 `#141316`、`#1B1A1E`、`#242328`、浅色文字及 `#F46A6C`。标识来自 `crates/mycode-desktop/assets/icon.svg`，生成器负责 `wiki/images/logo.svg` 和 `wiki/images/banner.svg`；桌面 ICO、PNG、ICNS 的生成另行处理。

<a id="build-and-validate"></a>

## 构建与检查

在仓库根目录运行，仅依赖 Python 标准库：

```sh
python scripts/build_wiki.py
python scripts/build_wiki.py --check
python -B -m unittest discover -s wiki/tests -v
```

`--check` 比较当前输入与已生成快照；图谱、架构正文、页面模板、任一内嵌源码或品牌素材变化都需要重新生成。生成时会拒绝不存在的源码路径、非法分栏和断开的节点关系。应在后端与界面改动整合后再生成并运行页面检查；以上是维护命令，不表示本次审计已执行它们。

<a id="interaction-and-accessibility"></a>

## 导航与无障碍操作

桌面宽度并排显示依赖图和选中节点详情。平板和手机默认显示节点列表，也可切换到图谱；手机选中节点后打开详情，返回操作恢复节点焦点。

搜索匹配节点名称、说明和源码路径，提供结果数和清除操作。按 `/` 聚焦搜索；图谱节点支持 Enter 与 Space。源码对话框提供复制、GitHub 链接、Escape 关闭及模态焦点处理；折叠导航支持 Escape 和打开时的 Tab 焦点约束。主题偏好在本地存储可用时保存。

旧节点与源码链接 `#n=<node>&f=<path>` 保持兼容。文档使用 `#v=architecture` 或 `#v=code-mode`，章节再附加 `&s=<heading-id>`。可见标题使用中文，改名的旧英文章节标识通过紧邻标题之前的独立 `<a id="old-english-slug"></a>` 保留；文档内部链接使用最终中文标题标识。
