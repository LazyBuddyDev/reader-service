# Reader API

API v1，JSON Lines私有持久会话。请求`{version:1,id:"...",method:"...",args:{...}}`；成功返回`{version:1,id,ok:true,data}`，失败返回`{version:1,id,ok:false,error:{code}}`。请求最大64KiB；不返回本机文件完整路径、正文诊断或原始异常。stdout只含协议。

## 数据归属与标识

服务数据根目录由`LAZYBUDDY_DATA_DIR`显式传入。`library.json`保存书库和偏好，`books/<book_id>/`保存书籍元数据、原文件副本`source.bin`、按每96块分文件保存的章节JSON及白名单图片。导入后原文件移动或删除不影响阅读；移除只处理受管理副本，绝不删除导入源。

书库条目`Entry`具有独立`id`，以及`source:{kind:"local",id:<文件SHA256>,format,encoding}`和`content_version`。内容版本由原文件标识、实际解码方式与内容模型版本生成。相同内容/解码重复导入返回已有条目；不同内容或不同解码不按同名覆盖。未来在线来源须使用独立来源标识与经过确认的章节映射，不自动合并同名书籍。

书籍`Book`含`id,title,author,source,content_version,chapters,resources,warnings`。章节`{id,title,blocks}`严格按解析后的阅读顺序排列；EPUB嵌套导航为每个spine资源提供首个有效标题，目录当前平铺为阅读章节，不保留完整多级导航树。

正文块`{id,kind,text,runs,resource}`；kind为paragraph、heading、pre或image。文本run含`text,strong,emphasis,link?`，只有书内片段链接作为非执行元数据保留。图片resource为内容哈希，缺失或拒绝图片使用替代文字。不得将正文当HTML执行。

位置`Position={book_id,content_version,chapter_id,block_id,offset}`，offset为Unicode标量值数量。ID只在对应内容版本内稳定。旧内容版本拒绝写入；客户端不能把换源后的章节和位置直接套入新版本。

## 方法

| 方法 | 参数与结果 |
| --- | --- |
| `health` | `{}` → service、api_version、当前能力列表 |
| `library.list` | `{start?:0,limit?:100}` → `{entries,total,last_book}`；limit最多100 |
| `library.import` | `{path:<用户选择的绝对路径>,encoding?:"auto"}` → `{job,status:"pending"}`；一次一个导入任务 |
| `import.status` | `{job}` → pending / cancelled / failed（error为错误码）/ complete（result含book、duplicate、warnings） |
| `import.cancel` | `{}` → `{}`；请求取消，尚未提交的内容不进入书库 |
| `lifecycle.suspend` | `{}` → `{}`；取消正在解析的导入，不启动后续任务 |
| `library.remove` | `{book_id}` → `{}`；先原子移除书库引用，再清理副本 |
| `book.open` | `{book_id}` → `{book,position,revision}` |
| `chapter.read` | `{book_id,chapter_id,start?:0}` → `{blocks,start,total}`；每次最多96块，start必须是96的倍数 |
| `chapter.find` | `{book_id,chapter_id,query,start?:0}` → `{match:null或{index,block_id,offset}}`；首个命中，query最多200字符 |
| `resource.read` | `{book_id,resource_id}` → `{url:<受限图片data URL>}`；由可见客户端按需请求 |
| `progress.save` | `{position,revision}` → `{saved:true,revision}`；仅接受严格较新的revision，客户端先读当前revision并串行保存 |
| `preferences.get` | `{}` → `{font_size,line_height,width}` |
| `preferences.set` | 完整偏好对象 → 已保存对象；字号14–36，行距1.3–2.5，宽度320–1000 |

导入解析在工作线程执行，查询接口不等待整个解析。完成状态读回才进行当前文件的有序提交；挂起会丢弃未提交结果。多文件队列归客户端，离页取消剩余队列；取消不撤销已经完成的导入。服务退出时请求取消，不派生任务。

## 实验性在线搜索

当前只实现番茄官方网页搜索适配，不在`health.capabilities`中声明可用。尚无真实成功响应验收，宿主尚未开放入口；详情、目录、正文、加入书架、账号及追更均未接入。样例测试通过不代表线上可用。

| 方法 | 参数与结果 |
| --- | --- |
| `online.search` | `{source:"fanqie",query,page?:0}` → `{job,status:"pending"}`；关键词去首尾空白后1–100字符，拒绝控制字符，page为0–99整数；每页10项 |
| `online.status` | `{job}` → pending / cancelled / failed（error为脱敏错误码）/ complete（result见下） |
| `online.cancel` | `{job}` → `{}`；只取消匹配任务，不影响其他任务 |

完成结果为`{source:"fanqie",entries:[{id,title,author,description}],page,has_more}`。ID保持官方数字字符串，不转浮点数，也不作为本地书籍ID。输出仅包含白名单元数据，不转发封面地址、验证信息或其他上游字段。搜索不读写书库、进度或凭据，不自动请求封面、详情、下一页或正文。

搜索在独立线程执行；全服务同时最多一个实际在途搜索，未结束时返回`search_busy`。挂起和退出请求取消，迟到结果不可读回；取消不保证立即中断底层阻塞I/O，该请求在至多15秒的全局超时内继续占用并发。新任务替代已结束任务后，旧job的状态/取消请求返回`job_not_found`，不会取消新任务。导入与搜索各有独立状态。

仅请求固定HTTPS端点`https://fanqienovel.com/api/author/search/search_book/v1`，使用NativeTls和当前系统代理，不接受任意URL、不跟随重定向、不重试、不注册设备、不使用第三方正文代理、不绕过验证。连接超时8秒、单次读取5秒、全局15秒，响应最多1MiB。保留`query_word`、`page_index`、`page_count=10`、`query_type=0`及默认filter参数；依据[官方搜索页面](https://fanqienovel.com/search/%E6%83%8A%E9%B8%BF)加载的公开客户端脚本核对字段，并非官方承诺稳定的开放API。

`source_verification_required`表示响应头要求人机验证；`source_empty_response`表示无验证头的空体；401/403为`source_access_denied`，429为`source_rate_limited`。其他错误包括`source_timeout`、`source_network_failed`、`source_request_rejected`、`invalid_source_response`。以上均不当作无结果；只有code=0且结构有效的空列表才表示空结果。

## 边界与错误

单文件最多32MiB，EPUB最多10000条归档记录，声明解压总量最多128MiB。XML单文件最多8MiB/300000节点，内容树深度最多128，书籍最多10000章/200000正文块；文本块最多1024字符。书库最多5000本，书库索引读取上限8MiB。PNG/JPEG/WebP图片每张最多1MiB、边长4096、像素800万；每书最多500张/32MiB。其他图片显示替代文字。本地阅读请求只读受管理内容，不加载外部资源；实验性在线搜索的网络边界见上节。

TXT自动识别UTF-8和带BOM的UTF-16；无法确定时返回`encoding_required`。可选utf-8、utf-16le、utf-16be、gb18030（包含常见GBK文本）、big5；解码错误不以替换字符悄悄吞掉。无标题TXT使用“正文”，超长章节通过chapter.read分块。

EPUB按spine阅读，使用EPUB3导航或EPUB2 NCX标题。只读取受控归档项，不将任意ZIP路径解压到文件系统。拒绝越界路径、链接条目、重复归档项、加密和固定版式；XML不解析实体声明，不执行脚本。ZIP/XML损坏明确失败。

常见错误：`file_unreadable`、`file_too_large`、`encoding_required`、`invalid_text_encoding`、`encrypted_epub`、`fixed_layout_epub`、`unsafe_archive`、`archive_too_large`、`invalid_xml`、`save_failed`、`invalid_saved_data`、`content_version_changed`、`stale_progress`。`removal_cleanup_pending`表示书库引用已移除但文件清理失败，不能当作完整删除成功。索引读取失败保留原文件，保存失败不提交新内存状态。

依赖来源、版本和许可证见Cargo.lock及包内THIRD-PARTY.md。实现依据：[roxmltree解析选项](https://docs.rs/roxmltree/0.20.0/roxmltree/struct.ParsingOptions.html)、[zip归档读取](https://docs.rs/zip/8.6.0/zip/read/struct.ZipArchive.html)、[encoding_rs编码接口](https://docs.rs/encoding_rs/0.8.35/encoding_rs/struct.Encoding.html)。

阅读偏好增加 `page_mode`，取值为 `horizontal`（左右翻页，默认）、`vertical`（上下翻页）或 `scroll`（连续滚动）。旧偏好缺少该字段时使用默认值；非法值拒绝且保留原文件。分页、窗口重排及按需分块衔接由客户端完成，服务仍按96块读取，位置仍使用章节/块/Unicode字符锚点，不保存像素坐标或页码。
