# Reader service

Windows x64独立本地阅读服务，无需额外安装Python或Lua环境，独立正文辅助程序内嵌LuaJIT。主服务采用MIT许可；独立正文辅助程序采用GPL-3.0-only，随模块提供其许可及对应源码。

支持TXT和无DRM可重排EPUB的导入、书库、目录、分块正文、章内检索、图片、阅读偏好与进度。书籍ID、来源标识和内容版本分离；客户端使用章节/正文块/Unicode字符偏移定位，不依赖屏幕像素。另提供七猫中文网、书旗小说官方访客来源的搜索、完整目录和免费章节阅读，以及番茄App公开匿名搜索（已验证真实两页结果）及官网公开详情/完整目录（已验证4423章），复用有界网络与异步任务，不增加浏览器或Python。在线书籍可保存到本应用书架、离线查看目录并移除；App正文按需接入既有分块、查找及进度模型，首章和第11章、重启恢复已验证；在线正文仅保留有界内存缓存。七猫/书旗的受限章节明确报错，不将付费试读或推荐正文当作完整章节。追更、跨来源聚合/换源、账号和同步尚未接入。

本地导入正文保留标题、段落、基础粗体/斜体及图片。复杂CSS、字体、音视频、固定版式、DRM、交互脚本和EPUB任意布局不在当前支持范围；超长段落拆块后只保留文本。书内片段链接保留为内容信息，当前客户端不提供脚注跳转。

```powershell
cargo test --locked
cargo test --manifest-path helpers/fanqie/Cargo.toml --locked
powershell -NoProfile -File scripts/package.ps1 -Destination <绝对包暂存根目录>
```

打包输出`<暂存根目录>/reader/0.1.12/`完整包及`target/packages/reader-0.1.12.zip`，包含精确依赖对应的许可文件。已存在的不可变包不覆盖，准备失败的`.pending`目录需核对后清理。测试临时数据在`tmp/`；源码不应作为构建缓存删除。

启动前必须设置独立持久目录`LAZYBUDDY_DATA_DIR`，API见[模块API](docs/module-api.md)。协议使用私有stdin/stdout，每行一个JSON；服务不输出日志；只有显式搜索、打开在线详情或准备章节时请求相应的固定官方HTTPS端点。宿主拥有服务进程树与生命周期。
