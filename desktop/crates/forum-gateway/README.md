# Forum Gateway：F10 局域网实现

桌面启动时只有既有 loopback 大屏。局域网 HTTPS 必须由操作员在「会议工作台 → 活动与会场共享」显式启用。关闭共享或退出应用后，当前浏览器授权与对端授权全部失效。模型任务、录音、逐字稿和设备控制没有对应的网络路由。

## 两会场操作

1. 来源电脑选择当前场次，在「参会者入口 / 连接另一会场」填写其会场 Wi-Fi IP，生成证书。生成器使用 macOS 自带 `/usr/bin/openssl`，在应用数据目录内创建独立 CA 和服务证书；CA 私钥生成后删除。不会安装系统信任，也不会下载依赖。证书生成可取消，子进程有 20 秒上限。
2. 手机系统安装导出的 `ca.cer`，核对 UI 中的 CA SHA-256。iPhone 手动安装证书后，还要在「设置 → 通用 → 关于本机 → 证书信任设置」启用完全信任；[Apple 官方说明](https://support.apple.com/102390)。Android 在系统安全/证书设置中安装 CA，受管理设备由管理员分发；[Google 官方说明](https://support.google.com/pixelphone/answer/2844832)。会场网络须允许设备互访。出现证书警告应修复 CA、IP、证书有效期或网络配置。
3. 操作员核对证书部署方式后启用 HTTPS；填写**明确审核过的公开场次名和会场名**。不会自动复制私有会议标题。参会者入口只读，15 分钟至 8 小时可选。QR 内容为单场 `https://IP:PORT/display.html?session=UUID#token=…`，没有控制 token；页面取出 fragment 后从地址栏移除，并按 tab/session 保存以支持刷新。
4. 电脑间使用「创建一次性会场邀请」，将邀请内容交给另一台操作员电脑。接收方在来源电脑上核对服务证书 SHA-256，并输入完整指纹后配对。邀请 5 分钟有效、仅可使用一次；兑换后最长 8 小时。邀请 ID 即最终授权 ID，来源电脑可随时撤销。
5. 两台独立安装的初始活动 UUID 不同。配对不会改写旧会议。需要组成同一活动时，接收方显式点击「将后续场次加入此活动」，停止采音后再新建场次；旧场次继续属于原活动。
6. 操作员在活动总览选择精确的已发布版本生成闭幕草稿。离线来源、已变化/撤回的版本会取消选择。草稿需要审核并填写公开正文才能展示/朗读。朗读前停止采音。主持人问题与脱敏检查保持私有。

服务证书有效 7 天、CA 有效 30 天。重启应用保留同一已保存证书，但不会自动开启共享、恢复访问 token 或恢复对端连接。IP 改变或证书过期时重新生成/部署。当前阶段手机、另一台 Mac 和实际会场网络验收仍待执行；本机 TLS 测试不替代这些验收。

## 认证与权限

- 参会者 `Participant`、一次性 `Invite`、对端 `Peer` 是独立能力类型；浏览器能力不能兑换邀请或读取 peer 路由，peer 能力不能当浏览器能力使用。
- 对端客户端验证来源端 CA、主机名、有效期、TLS 握手签名，另外固定来源服务证书的 SHA-256。服务端验证独立 bearer，并绑定 recipient device ID 和单场 scope。
- 这里采用**固定来源 TLS 身份 + 独立的对端只读 bearer**，不声称实现双向客户端证书 mTLS。
- 任意请求精确验证 Host；浏览器 Origin 必须与当前 HTTPS origin 相同，peer 路由拒绝浏览器 Origin。没有 CORS、控制命令代理或任意文件读取。
- 公共正文、引用、搜索仅来自 `PublicSnapshot` 中的审核副本；所有 peer 响应为 `deny_unknown_fields` 的契约。页面将内容作为文本渲染。
- 访问权限清单不显示 token，只显示 ID、公开来源、用途和到期时间，方便切换场次后撤销旧入口。

## 路由与恢复

| 路由 | 凭据与内容 |
|---|---|
| `GET /display.html` 与构建静态资源 | 不包含任何会议信息的页面壳 |
| `GET /v1/display/sessions/{id}/snapshot` | 本场 Participant bearer；完整公开替换快照 |
| `GET /v1/display/sessions/{id}/search?q=…` | 同一 Participant bearer；公开正文/引用文字匹配，最多 100 项 |
| `POST /v1/peer/pair` | 一次性邀请，兑换绑定接收设备的只读 lease |
| `GET /v1/peer/sessions/{id}/snapshot` | Peer bearer + `X-Forum-Device`；来源 owner/event/session 与公开快照 |
| `GET /v1/peer/sessions/{id}/changes?after=…` | 同一 Peer 授权；来源发布游标增量 |

宿主后台每轮按已配对源拉取增量。Core 检查 owner/session、精确前游标、重复、乱序、变更版本与撤回水位；不匹配则请求权威全快照。网络断开将该来源标记 stale 并撤下依赖它的本地公开成果。旧副本仍可在操作员活动页查看，并标出最后同步时间，但不能进入新分析。断开连接、关闭共享或更换 client 后，旧网络响应不能再次提交为当前连接结果。

只读浏览器每 2 秒取完整公开快照。连接失败时隐藏可见内容；恢复后用完整快照替换。参会者页面检索当前已授权的公开副本；Rust search 路由同时可用。HTTP 均为 `no-store`，fragment 凭据不会送入普通请求 URL 或 referrer。

## 资源限制

进程范围内 loopback/TLS 共用 8 个连接/reader 槽位；卡死 callback 保留槽位，防止重建服务绕开限制。每个实际 TLS socket 读写都重新计算 6 秒绝对期限，防止慢速握手延长超时。Header 最多 16 KiB、请求正文最多 4 KiB、公开响应最多 16 MiB。每个已授权 token 每 10 秒最多 30 次读取，最多 64 个未过期授权，最多 8 个主动同步对端。

关闭服务先撤销授权、关闭在途 socket，再 join 自己的连接线程。reader 在返回前、HTTP 输出分块前都会重新检查授权。不能强行终止任意 Rust callback，但这样的 callback 始终占用全局预算；生产 callback 只读 Core actor。

## 本机验证

- `cargo +stable test --locked --offline -p forum-gateway --lib`：保留的 5 项 loopback 测试，加 TLS pinning/CA 检查、单次邀请、peer/浏览器隔离、单场限制、公开搜索、撤销/过期、callback 内撤销、速率预算、慢速 TLS 绝对期限、退出关闭在途 TLS。
- `cargo +stable test --locked --offline -p forum-shell lan_manager::tests`：临时两数据库 + 真实 TLS，验证显式启用、私有标题不外传、原有活动与后续场次活动隔离、撤销 stale、重配恢复、断开后的旧结果围栏、证书持久化且重启不自动监听。
- `npm run check && npm run test:p0 && npm run build`：Svelte/TypeScript、精确版本选择失效、只读搜索 scope、说话人 session/source revision 围栏，以及既有转译 UI 回归。

以上测试只用合成文本/临时证书与 loopback，不采集麦克风，不公开真实会议，也不安装 CA 到本机或手机信任库。
