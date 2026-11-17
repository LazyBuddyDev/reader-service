# Reader API

API v1，JSON Lines私有持久会话。请求`{version:1,id:"...",method:"...",args:{...}}`；成功返回`{version:1,id,ok:true,data}`，失败返回`{version:1,id,ok:false,error:{code}}`。请求最大64KiB；不返回本机文件完整路径、正文诊断或原始异常。stdout只含协议。

## 数据归属与标识

服务数据根目录由`LAZYBUDDY_DATA_DIR`显式传入。`library.json`保存书库和偏好，`books/<book_id>/`保存书籍元数据、原文件副本`source.bin`、按每96块分文件保存的章节JSON及白名单图片。导入后原文件移动或删除不影响阅读；移除只处理受管理副本，绝不删除导入源。

书库条目`Entry`具有独立`id`，以及`source:{kind:"local",id:<文件SHA256>,format,encoding}`和`content_version`。内容版本由原文件标识、实际解码方式与内容模型版本生成。相同内容/解码重复导入返回已有条目；不同内容或不同解码不按同名覆盖。在线书籍使用来源类型和来源书籍ID去重，不自动合并同名书籍。

书籍`Book`含`id,title,author,source,content_version,chapters,resources,warnings`。章节`{id,title,blocks}`严格按解析后的阅读顺序排列；EPUB嵌套导航为每个spine资源提供首个有效标题，目录当前平铺为阅读章节，不保留完整多级导航树。

正文块`{id,kind,text,runs,resource}`；kind为paragraph、heading、pre或image。文本run含`text,strong,emphasis,link?`，只有书内片段链接作为非执行元数据保留。图片resource为内容哈希，缺失或拒绝图片使用替代文字。不得将正文当HTML执行。

位置`Position={book_id,content_version,chapter_id,block_id,offset}`，offset为Unicode标量值数量。ID只在对应内容版本内稳定。旧内容版本拒绝写入；客户端不能把换源后的章节和位置直接套入新版本。

## 方法

| 方法 | 参数与结果 |
| --- | --- |
| `health` | `{}` → service、api_version、当前能力列表 |
| `library.list` | `{start?:0,limit?:100}` → `{entries,total,last_book}`；仅返回在书架中的条目，过滤后分页，limit最多100 |
| `library.import` | `{path:<用户选择的绝对路径>,encoding?:"auto"}` → `{job,status:"pending"}`；一次一个导入任务 |
| `import.status` | `{job}` → pending / cancelled / failed（error为错误码）/ complete（result含book、duplicate、warnings） |
| `import.cancel` | `{}` → `{}`；请求取消，尚未提交的内容不进入书库 |
| `lifecycle.suspend` | `{}` → `{}`；取消正在解析的导入，不启动后续任务 |
| `library.remove` | `{book_id}` → `{}`；先原子移除书库引用，再清理副本 |
| `book.open` | `{book_id}` → `{book,position,revision,in_shelf}` |
| `library.favorite` | `{book_id,in_shelf:<bool>}` → `{book:<Entry>}`；原子切换本应用书架状态，保留副本、目录及进度 |
| `chapter.read` | `{book_id,chapter_id,start?:0}` → `{blocks,start,total}`；每次最多96块，start必须是96的倍数 |
| `chapter.find` | `{book_id,chapter_id,query,start?:0}` → `{match:null或{index,block_id,offset}}`；首个命中，query最多200字符 |
| `resource.read` | `{book_id,resource_id}` → `{url:<受限图片data URL>}`；由可见客户端按需请求 |
| `progress.save` | `{position,revision}` → `{saved:true,revision}`；仅接受严格较新的revision，客户端先读当前revision并串行保存 |
| `preferences.get` | `{}` → `{font_size,line_height,width}` |
| `preferences.set` | 完整偏好对象 → 已保存对象；字号14–36，行距1.3–2.5，宽度320–1000 |

导入解析在工作线程执行，查询接口不等待整个解析。完成状态读回才进行当前文件的有序提交；挂起会丢弃未提交结果。多文件队列归客户端，离页取消剩余队列；取消不撤销已经完成的导入。服务退出时请求取消，不派生任务。

## 在线搜索（番茄 App）

`health.capabilities`包含`online_search`。当前支持番茄App的公开匿名书籍元数据搜索；真实第一页及服务端游标下一页均已验证，每页10项。不要求登录、Cookie、设备注册或签名；不增加Python或浏览器运行时。详情和目录的独立能力见下节；搜索成功不代表正文、书架写入或章节权限通过。

| 方法 | 参数与结果 |
| --- | --- |
| `online.search` | `{source:"fanqie",query,cursor?:0}` → `{job,status:"pending"}`；关键词去首尾空白后1–100字符，拒绝控制字符，cursor为0–4294967295整数 |
| `online.status` | `{job}` → pending / cancelled / failed（error为脱敏错误码）/ complete（result见下） |
| `online.cancel` | `{job}` → `{}`；只取消匹配任务，不影响其他任务 |

完成结果为`{source:"fanqie",entries:[{id,title,author,description}],cursor,next_cursor}`。`next_cursor`为服务端返回的下一偏移量，末页为null；客户端不得按结果条数推算。结果不超过50项，保留数字字符串ID，不转浮点数，也不作为本地书籍ID。仅输出白名单元数据，不转发封面、跳转、search_id或其他上游字段；同名书籍不自动合并。搜索不读写书库、进度或凭据，不自动获取封面、详情、下一页或正文。

搜索在独立线程执行；全服务同时最多一个实际在途搜索，未结束时返回`search_busy`。挂起和退出请求取消，迟到结果不可读回；取消不保证立即中断底层阻塞I/O，该请求在至多15秒的全局超时内继续占用并发。新任务替代已结束任务后，旧job的状态/取消请求返回`job_not_found`，不会取消新任务。导入与搜索各有独立状态。

固定端点为`https://novel.snssdk.com/api/novel/channel/homepage/search/search/v1/`；参数为`device_platform=android`、`parent_enterfrom=novel_channel_search.tab.`、`aid=1967`、`q`和`offset`。复用ureq的参数编码、NativeTls及系统代理，不接受任意URL、不跟随重定向、不重试、不注册或轮换设备。连接超时8秒、单次读取5秒、全局15秒，响应最多1MiB。请求形状参考[已有番茄App适配](https://github.com/lingyi-ling/novel-downloader/blob/main/src/novel_downloader/plugins/sites/fanqienovel/searcher.py)和[已有书源的匿名搜索说明](https://github.com/gs1147/fanqie-booksource/blob/main/搭建指南.md)，并以真实响应验证；不是官方承诺稳定的开放API。当前端点无需签名，因此不移植上游签名、设备注册或正文解密代码。

`source_verification_required`表示响应头要求官方验证；`source_empty_response`表示无验证头的空体；401/403和重定向为`source_access_denied`，429为`source_rate_limited`。其他错误包括`source_timeout`、`source_network_failed`、`source_response_too_large`、`source_request_rejected`、`source_text_obfuscated`、`invalid_source_response`。私用字体字符拒绝展示，不把乱码冒充可读元数据。以上均不当作无结果；只有code=0且结构有效的空列表才表示空结果。

宿主仅在可见内容页接受上述方法，代数与现有阅读请求一致；设置页不可搜索。书架、在线列表和设置各保留自己的状态；在线输入明确提交才请求，前后分页替换当前结果页，离开取消且不自动恢复搜索。失败保留已确认列表，换根页面、设置和隐藏后迟到结果不能更新列表；恢复只显示已有状态。

## 在线详情与目录（官方公开元数据）

`health.capabilities`包含`online_details`。`online.detail {source:"fanqie",book_id}`返回`{job,status:"pending"}`，复用`online.status`及`online.cancel`。搜索与详情共用一个真实在途任务；切换、挂起或退出不派生后续请求，取消后迟到结果不可交付。标识为1–24位ASCII数字字符串，传给固定URL前验证，不接受任意URL。

完成结果为`{source:"fanqie",id,title,author,description,chapters:[{id,title}]}`。字段中的ID均为来源ID，不转换成宿主本地书籍ID；详情本身不写书架；用户明确加入时按下节持久化目录快照。只返回白名单元数据，不返回封面、账号、上游签名或网页锁定字段；网页锁定状态不能代替App章节权限。

详情只请求`https://fanqienovel.com/page/<book_id>`官方公开页面。真实验证已读回《惊鸿》的4423章完整目录。使用与搜索相同的NativeTls、系统代理及有界超时；响应上限4MiB。只通过serde_json解析`window.__INITIAL_STATE__`赋值中的第一个JSON值，不执行网页JavaScript或请求其资源。书籍ID必须匹配请求；标题、作者和简介的上限与搜索一致，目录最多10000章，章节ID唯一并保留服务端卷序/章序；实际章数必须等于声明总数。缺失章节返回`incomplete_source_directory`，结构错误或重复ID返回`invalid_source_response`，不能当作空目录。

宿主在用户明确点击搜索结果的“详情”后请求，不预取其他书籍。目录每页只渲染100章，分页只切本地已验证快照，不联网。返回结果、切换根页面或设置会保留搜索快照及滚动；已载入目录和展示页保留，载入中离开则取消，重新进入只显示保留内容，由用户明确选择“载入目录”再请求。

此能力只提供书籍详情与目录，不表示正文可读。已对旧匿名App目录及首章正文端点各进行一次核验，均为HTTP200空体，未使用账号、设备注册、内容密钥或版本降级；原因未知，不推断为付费、账号限制或服务端封禁。用户确认搜索已可用，正文仍需验证兼容的正常客户端/会话及授权范围；不通过换设备、降级或第三方正文代理补齐。

## 边界与错误

单文件最多32MiB，EPUB最多10000条归档记录，声明解压总量最多128MiB。XML单文件最多8MiB/300000节点，内容树深度最多128，书籍最多10000章/200000正文块；文本块最多1024字符。书库最多5000本，书库索引读取上限8MiB。PNG/JPEG/WebP图片每张最多1MiB、边长4096、像素800万；每书最多500张/32MiB。其他图片显示替代文字。本地阅读请求只读受管理内容，不加载外部资源；实验性在线搜索的网络边界见上节。

TXT自动识别UTF-8和带BOM的UTF-16；无法确定时返回`encoding_required`。可选utf-8、utf-16le、utf-16be、gb18030（包含常见GBK文本）、big5；解码错误不以替换字符悄悄吞掉。无标题TXT使用“正文”，超长章节通过chapter.read分块。

EPUB按spine阅读，使用EPUB3导航或EPUB2 NCX标题。只读取受控归档项，不将任意ZIP路径解压到文件系统。拒绝越界路径、链接条目、重复归档项、加密和固定版式；XML不解析实体声明，不执行脚本。ZIP/XML损坏明确失败。

常见错误：`file_unreadable`、`file_too_large`、`encoding_required`、`invalid_text_encoding`、`encrypted_epub`、`fixed_layout_epub`、`unsafe_archive`、`archive_too_large`、`invalid_xml`、`save_failed`、`invalid_saved_data`、`content_version_changed`、`stale_progress`。`removal_cleanup_pending`表示书库引用已移除但文件清理失败，不能当作完整删除成功。索引读取失败保留原文件，保存失败不提交新内存状态。

依赖来源、版本和许可证见Cargo.lock及包内THIRD-PARTY.md。实现依据：[roxmltree解析选项](https://docs.rs/roxmltree/0.20.0/roxmltree/struct.ParsingOptions.html)、[zip归档读取](https://docs.rs/zip/8.6.0/zip/read/struct.ZipArchive.html)、[encoding_rs编码接口](https://docs.rs/encoding_rs/0.8.35/encoding_rs/struct.Encoding.html)。

阅读偏好增加 `page_mode`，取值为 `horizontal`（左右翻页，默认）、`vertical`（上下翻页）或 `scroll`（连续滚动）。旧偏好缺少该字段时使用默认值；非法值拒绝且保留原文件。分页、窗口重排及按需分块衔接由客户端完成，服务仍按96块读取，位置仍使用章节/块/Unicode字符锚点，不保存像素坐标或页码。

## 在线书架

`health.capabilities`包含`online_shelf`。`library.add {source:"fanqie",book_id}` → `{book:<Entry>,duplicate}`，只持久化本应用书架，不写官方账号收藏。新的条目只能来自当前服务已完成、已读回且ID匹配的`online.detail`结果；不接受客户端提交标题、目录、正文或URL。缺少匹配快照返回`source_detail_required`，客户端需重新载入目录。已在书架的相同来源ID直接返回已有条目，不覆盖快照、进度或更新时间；不同来源ID即使同名也分别保存。

复用现有原子书库提交、最多5000本限制及移除流程。条目source为`{kind:"fanqie",id:<来源书籍ID>,format:"online",encoding:""}`；本地ID由来源类型和来源ID生成稳定哈希。`book.open`返回已保存的简介及完整有序目录，无网络请求；新增`Book.description`为可选字段，旧本地书籍缺省为空。目录章节ID保留来源ID，`blocks:0`表示尚无正文，`catalog-<hash>`仅标识目录快照，不声称是已验证的正文版本。

书架保存不下载正文、不创建空原文件或章节文件。`online_body`能力及按需正文协议见下节；章节未准备时返回`chapter_not_loaded`，不能把空正文冒充读取成功。宿主从在线详情星标加入或移出书架，确认提交后更新星标；支持正文的宿主可从书架条目恢复阅读，也可在在线详情选择章节直接阅读，首次阅读不自动收藏。写入期间禁重复操作，保存失败保留目录和原书架，可由用户明确重试。离页后的写入结果不更新旧视图，再进入时只读核对书架；恢复不会重复提交。

正文已验证《惊鸿》首章和第11章，后续章节权限和稳定性仍需继续核验，公开目录成功不能外推正文成功。未加入设备轮换、版本降级或第三方正文代理；未实现官方云端书架、目录更新、未读/追更或正文缓存。

## App正文协议与验证

隔离验证已确认：匿名正文探测的HTTP200是上游明确声明`Content-Length: 0`，不是JSON解析或解压丢失正文。相同请求在当前网络与禁用显式代理的对照中均为空，搜索控制请求返回9046字节；当前客户端使用的App网关在无身份、无签名条件下同样返回空体。不能把空体直接命名为“设备作废”、付费限制或全站故障。

复用[fanqie.koplugin的reading实现](https://github.com/hesan1232/fanqie.koplugin/blob/9ba6f8151a24815be1c5a6917750704031defa96/fanqie/reading.lua)，使用其支持的7.3.7.33画像、单一临时注册身份和完整签名，最小链路已通过：

| 步骤 | 固定端点 | 实际结果 |
| --- | --- | --- |
| 初次设备注册 | `https://log.snssdk.com/service/2/device_register/` | HTTP200，384字节，取得临时身份 |
| 内容密钥注册 | `https://api5-normal-sinfonlinec.fqnovel.com/reading/crypt/registerkey` | HTTP200，128字节，业务code=0 |
| 单章读取 | `https://api5-normal-sinfonlinec.fqnovel.com/reading/reader/batch_full/v` | HTTP200，27320字节，业务code=0；正文解析为3172个可读字符，私用区/替换字符均为0 |

请求签名由上游实现生成X-Khronos、X-Ladon、X-Helios、X-Argus；不能拿App搜索的匿名参数直接拼成正文请求。请求形状、设备身份、签名和密钥注册共同补齐后首章可读，可以把故障定位到原探测缺少正常App协议上下文。当前对照没有单独消融各个签名头和身份参数，不声称已经确定某一个头是唯一原因，也未证实旧域名在完整协议下是否仍可用。

最初的隔离协议验证只读取《惊鸿》首章，每个协议阶段最多一次请求，不激活会员、不轮换身份、不降级重试，也没有账号Cookie。临时身份、内容密钥和明文只驻留进程内存；服务端时间与本机差小于1秒。离线检查覆盖AES-CBC NIST向量、SM3 abc向量、Base64及有界解压。仅首章成功，不能外推后续章节权限、账号能力、长期稳定性或全书完整性。

### 正式接入的复用选择

| 方案 | 收益 | 成本与边界 |
| --- | --- | --- |
| 独立LuaJIT辅助进程复用原SDK（优先） | 保留已读通的协议实现，reader通过固定私有协议调用，避免重新编写签名与正文处理 | 已落实Windows独立构建、固定API/端点、安全存储和取消/进程树验证；依上游GPLv3许可保留来源与源码，不直接复制进现有MIT服务 |
| 独立Python＋Lupa辅助进程 | 已在Windows研究环境读通，适合继续验证协议与章节 | 引入额外Python运行时及依赖维护；本次仅作研究工具，未加入产品或包清单 |
| 将协议整体移植到Rust | 可沿用服务语言及部署方式 | 需要重新验证多种签名算法和正文处理，增加维护面，违背优先复用的当前目标，不作为首选 |

上游源码来自固定提交`9ba6f8151a24815be1c5a6917750704031defa96`，许可见其[LICENSE](https://github.com/hesan1232/fanqie.koplugin/blob/9ba6f8151a24815be1c5a6917750704031defa96/LICENSE)。研究环境使用PyPI的`lupa==2.6`提供LuaJIT；上游协议算法未改写，只通过适配层提供Windows系统随机数、标准MD5、JSON/HTTPS和有界解压。正式实现使用独立的原生Rust＋LuaJIT辅助程序，固定协议、运行时及源码许可随完整包交付；Python研究环境不进入模块。主服务仍为MIT，辅助程序目录显式采用GPL-3.0-only。

## 按需在线正文

`health.capabilities`包含`online_body`。`online.chapter {book_id:<受管理书籍ID>,chapter_id:<来源章节ID>}`仅接受本应用已保存且目录中存在的番茄章节。已在内存中时直接返回`{status:"complete",result:{book_id,chapter_id,chapter_version,total}}`；否则返回`{job,status:"pending"}`并通过既有`online.status`读回相同result。搜索、详情和正文共用一个实际在途任务；正文准备完成只交付章节元数据，随后复用`chapter.read`、`chapter.find`读取/检索正文块，不一次向界面交付整本书。

`chapter.read`对在线章节增加`chapter_version`，本地章节为null。在线`Position`增加可选`chapter_version`，由当前规范化章节块计算SHA256；`content_version`继续表示保存的目录快照，两者不能互相代替。在线进度必须匹配实际已准备的章节版本，旧正文锚点返回`content_version_changed`；宿主重新打开后核对版本，有变化则回到本章开头。字符偏移仍为Unicode标量值，不能按字节或像素存储。旧本地位置无需该字段，保持兼容。

正文只在有界内存缓存中保留最近3章，不写入`book.json`、章节JSON、原文件、日志或诊断；退出后需按用户明确打开重新取本章。HTML通过标准解析库规范化为既有Block/Run，保留标题、段落、基本粗斜体及换行；不执行脚本、不获取图片或其他嵌入资源。解析输入最多1MiB、50000个DOM节点、深度128、4096个正文块；私用字体和替换字符拒绝作为可读正文。仅用户明确打开/切章才准备正文，没有后续章节预加载。页内分块、查找和已缓存章节读取不再联网。

独立`bin/fanqie-helper.exe`只加载编入程序的固定SDK，不读取工作区脚本，不接受URL/eval/文件命令。Windows Credential Manager保存本机生成的固定画像、注册身份和内容密钥；隔离宿主传入独立`LAZYBUDDY_CREDENTIAL_SCOPE`，凭据读取或保存失败即报错，不覆盖为默认身份。无会员激活、账号Cookie、身份轮换或降级恢复。当前来源请求含SDK自身生成的设备Cookie（install_id），不读取浏览器或官方账号会话。固定端点、NativeTls、系统代理、超时、大小及重定向边界在辅助程序内约束。

正文任务取消时终止其辅助进程Job，至多25ms轮询间隔后停止等待；未提交的迟到结果不能进入缓存或界面。闲置辅助程序不轮询网络；服务退出释放Job，不遗留进程。已提交的凭据保存不能承诺回滚。401/403、验证要求或空响应停止当前辅助程序的后续取源；不重注册身份规避拒绝。其他网络失败也不自动重试，用户可明确重试同一书籍/章节。缺组件、网络、权限、正文解析及凭据错误分别返回脱敏代码。

完整0.1.9包在独立数据目录下验证《惊鸿》4423章入架后首章101块、第11章124块、分块/章内查找、同一身份和密钥的进程重启复用、位置恢复、旧正文版本拒绝及无正文文件落盘。单测另覆盖取消后不接受迟到正文、缓存上限和Job退出；界面样例覆盖加入后阅读、失败保留、设置切换及迟到隔离。真实服务、样例界面、实机联合验收分开报告，不外推全书、账号或长期稳定性。

模块包含辅助程序对应源码`sources/fanqie-helper-0.1.0.zip`及精确依赖许可；MPL依赖同时携带已发布的源码包。许可遗漏的补充来源见根`licenses/README.md`及辅助程序README，构建不从未固定分支下载代码。正式发布仍须按独立发布授权执行。

## 书架状态与直接阅读

`health.capabilities`中的`shelf_membership`表示支持独立书架状态。Entry的`in_shelf`持久化；旧索引缺省为true，沿用原书架。`online.open {source:"fanqie",book_id}`与`library.add`复用已验证详情保存逻辑，首次保存不加入书架；再次打开已有条目不改变书架状态。`library.add`和重复导入本地文件会重新加入已移出条目，保留原有进度。`library.favorite`只切换本应用书架状态，不写官方账号收藏；失败不提交内存，重复相同状态不写文件。移出后正文读取和位置保存仍可继续。`library.remove`仍是物理删除副本及记录的显式接口，不用于星标切换。
