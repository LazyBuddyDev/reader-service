# Reader service

Windows x64独立本地阅读服务，Rust编译后不依赖Python或其他解释器。MIT许可。

支持TXT和无DRM可重排EPUB的导入、书库、目录、分块正文、章内检索、图片、阅读偏好与进度。书籍ID、来源标识和内容版本分离；客户端使用章节/正文块/Unicode字符偏移定位，不依赖屏幕像素。当前仅提供本地来源；在线搜索、追更、账号和同步尚未实现。

正文保留标题、段落、基础粗体/斜体及图片。复杂CSS、字体、音视频、固定版式、DRM、交互脚本和EPUB任意布局不在当前支持范围；超长段落拆块后只保留文本。书内片段链接保留为内容信息，当前客户端不提供脚注跳转。

```powershell
cargo test --locked
powershell -NoProfile -File scripts/package.ps1 -Destination <绝对包暂存根目录>
```

打包输出`<暂存根目录>/reader/0.1.2/`完整包及`target/packages/reader-0.1.2.zip`，包含精确依赖对应的许可文件。已存在的不可变包不覆盖，准备失败的`.pending`目录需核对后清理。测试临时数据在`tmp/`；源码不应作为构建缓存删除。

启动前必须设置独立持久目录`LAZYBUDDY_DATA_DIR`，API见[模块API](docs/module-api.md)。协议使用私有stdin/stdout，每行一个JSON；服务不输出日志，不请求网络。宿主拥有服务进程树与生命周期。
