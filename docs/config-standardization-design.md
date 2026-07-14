# 配置标准化设计

## 目标

将 CLI 当前依赖的 `.env` 文件和进程环境变量配置，统一迁移到用户级 TOML
文件 `~/.mini-agent/config.toml`。新增专用配置模块，负责配置文件定位、解析、
默认值和校验；CLI 与后续可能新增的入口只消费一个经过类型化校验的配置契约。

本文仅为设计文档，不修改当前实现。

## 范围与约束

- 配置文件路径固定为 `~/.mini-agent/config.toml`。
- 现有 `.env` 中的 provider 配置必须迁移至 TOML；应用不再通过 `dotenvy`
  加载 `.env`。
- 进程环境变量不是配置来源，也不用于覆盖配置。这样可避免实际配置随启动终端
  而变化。
- 保持现有 provider 行为、默认值和可接受取值不变。
- 本次只负责配置文件化，不新增 `mini-agent config init` 等配置管理命令；配置文件
  由用户或部署流程直接创建。
- 密钥只保存在用户本地配置中，不得提交到 Git 仓库，不得写入包含真实密钥的
  示例文件，也不得输出到日志。
- 本次迁移覆盖当前二进制程序的五个 `OPENAI_*` 配置变量，并将日志级别配置
  显式迁移到 TOML。

## 现状调研

当前配置边界全部位于 `agent-cli`：

- `crates/agent-cli/src/main.rs:12` 调用 `dotenvy::dotenv()`，将工作目录中的
  `.env` 文件加载到进程环境中。
- `crates/agent-cli/src/main.rs:121` 至 `:149` 直接解析五个 `OPENAI_*` 变量。
  `OPENAI_API_KEY` 为必填项，其余字段有默认值或可选语义。
- `crates/agent-cli/src/main.rs:151` 至 `:157` 使用
  `tracing_subscriber::EnvFilter::try_from_default_env` 初始化日志过滤，因此
  `RUST_LOG` 是隐式配置输入。
- `crates/agent-cli/Cargo.toml:13` 声明了 `dotenvy` 依赖；当前不存在共享配置 crate。
- `README.md`、`docs/CONTRIB.md` 和 `docs/RUNBOOK.md` 均描述了 `.env` 与
  环境变量配置方式。

`provider` 和 `agent-core` crate 只接收已解析的配置值，自身不读取环境变量。
它们应继续不承担文件配置职责。

## 目标配置契约

`~/.mini-agent/config.toml` 使用显式层级结构，而不是沿用环境变量形式的键名：

```toml
[provider.openai]
# 必填。请确保此文件仅允许当前用户读取。
api_key = "your-api-key"

# 可选项，下面展示当前运行时默认值。
base_url = "https://api.openai.com/v1"
model = "gpt-5.5"
api_type = "completions"
reasoning_effort = "medium"

[logging]
level = "info"
```

`reasoning_effort` 可以省略，等价于当前未设置或为空的
`OPENAI_REASONING_EFFORT`。`logging.level` 也可省略，默认值为 `info`。注释只
用于说明，配置文件本身保持为标准 TOML。

### 迁移映射

| 当前输入 | TOML 键 | 是否必填 | 默认值或省略后的行为 |
| --- | --- | --- | --- |
| `OPENAI_API_KEY` | `provider.openai.api_key` | 是 | 缺失或为空时启动失败。 |
| `OPENAI_BASE_URL` | `provider.openai.base_url` | 否 | `https://api.openai.com/v1` |
| `OPENAI_MODEL` | `provider.openai.model` | 否 | `gpt-5.5` |
| `OPENAI_API_TYPE` | `provider.openai.api_type` | 否 | `completions` |
| `OPENAI_REASONING_EFFORT` | `provider.openai.reasoning_effort` | 否 | `None` |
| `RUST_LOG` | `logging.level` | 否 | `info` |

前五项与当前 `Config` 字段一一对应。日志配置也改为显式 TOML 字段，确保废弃
`.env` 后不存在隐藏的环境变量依赖。

## 建议的模块边界

在 `crates/agent-config` 新增名为 `agent-config` 的 workspace 成员。该 crate
拥有配置契约，并暴露精简的公共 API，例如：

```text
ConfigPath::default() -> ~/.mini-agent/config.toml
AppConfig::load_default() -> Result<AppConfig, ConfigError>
AppConfig::load_from(path) -> Result<AppConfig, ConfigError>
```

crate 内部应分为三层：

1. **路径解析**：解析用户主目录，拼接 `.mini-agent/config.toml`。若无法确定主
   目录，返回可操作的错误。不得查找当前工作目录。
2. **文件解析**：读取 UTF-8 TOML，并通过 `serde` 与 TOML 解析器反序列化为
   仅供解析使用的结构体。I/O 或语法错误必须带上已解析出的文件路径。
3. **校验与规范化**：将原始解析结果转换为公开的类型化 `AppConfig`，应用默认
   值、清理可选文本、拒绝非法值；`Display`、`Debug` 和错误信息中都不得包含
   API Key。

公共模型应使用 provider 无关的字符串或小型配置枚举。`agent-cli` 在组合根部将
校验通过的 `api_type` 和 `reasoning_effort` 转换为 `provider` 对应类型。依赖方向
保持单向：`agent-cli -> agent-config` 与 `agent-cli -> provider`；`provider` 和
`agent-core` 都不依赖配置 crate。

## 行为规范

### 启动顺序

1. CLI 解析 `~/.mini-agent/config.toml` 路径。
2. 在初始化 provider 和 tracing 之前，读取、解析、校验并规范化配置文件。
3. 使用 `logging.level` 初始化 tracing。
4. 使用校验通过的 OpenAI 配置构造 `OpenAIProvider`。

应用路径中不再调用 `dotenvy::dotenv`，不再为应用设置读取 `std::env::var`，
也不再使用 `EnvFilter::try_from_default_env`。同时从依赖图中移除 `dotenvy`。

### 必须执行的校验

- 配置文件必须存在且为可读普通文件。文件缺失时，错误信息需指出预期路径，并
  给出最小创建模板，但不得回显任何密钥内容。
- `provider.openai` 与 `provider.openai.api_key` 必须存在；`api_key` 去除空白后
  必须非空。
- `base_url` 必须是绝对 HTTP 或 HTTPS URL，不得包含查询参数或片段，且不得以
  `/chat/completions` 或 `/responses` 结尾。去除尾部 `/`，以维持当前请求 URL
  的拼接行为。
- `model` 去除空白后必须非空。
- `api_type` 仅接受 `completions` 和 `responses`。
- `reasoning_effort` 存在时仅接受 `low`、`medium`、`high` 和 `xhigh`；空 TOML
  字符串规范化为 `None`，以兼容当前行为。
- `logging.level` 使用 `tracing_subscriber::EnvFilter` 解析，以继续支持
  `info,provider=debug` 等 directive。非法过滤条件必须在进入交互循环前报错。

每个配置节都应通过 `serde(deny_unknown_fields)` 拒绝未知 TOML 键。端点或密钥
字段的拼写错误应尽早失败，而不是静默使用默认值。

### 错误展示

配置失败应通过 `main` 返回 `anyhow::Result`，而不是触发 panic。每个错误都应
包含已解析路径；可定位时包含错误配置键；枚举类配置应列出允许的取值。API Key
不得出现在任何错误或诊断输出中。

预期错误示例：

```text
未找到配置文件：/Users/alice/.mini-agent/config.toml
请创建该文件，并在 [provider.openai] 中配置非空 api_key。

配置无效，文件：/Users/alice/.mini-agent/config.toml
provider.openai.api_type 必须是以下值之一：completions、responses
```

## 备选方案

### 方案一：配置逻辑保留在 `agent-cli`

该方案的即时改动最小，但会继续将解析、校验、CLI 初始化和未来命令配置耦合在
`main.rs` 中，无法清晰满足新增专用配置模块的要求。

### 方案二：使用 `config` 等环境变量合并库

这类库通常会合并 TOML 与环境变量，需要引入优先级规则，与本次迁移要求的单一
配置源相冲突；同时也会增加定位实际生效配置来源的难度。

### 方案三：推荐，新增专注的 `agent-config` workspace crate

独立 crate 可供未来二进制入口复用，隔离文件系统和 TOML 依赖，并支持使用临时
路径进行确定性的单元测试。它只适度增加 workspace 结构，同时保留既有的
provider 与 core 边界。

## 内测阶段迁移策略

当前应用仍处于内测阶段，本次配置调整不需要提供旧配置格式兼容能力，也不需要
保留 `.env` 或环境变量回退逻辑。新版本直接以 `~/.mini-agent/config.toml` 作为
唯一配置来源；仓库 `.env`、启动目录 `.env` 以及导出的 `OPENAI_*`、`RUST_LOG`
变量均不参与配置加载。

内测用户只需手动创建一次配置文件：

1. 创建 `~/.mini-agent/`，在支持的系统上设置为仅所有者可访问。
2. 依照迁移映射，将现有 `.env` 中的值写入 TOML 键。
3. 删除旧 `.env`，其中包含凭据时应优先处理。
4. 运行 CLI，确认目标 provider 与日志配置。

实现不需要自动导入、转换或删除 `.env`。文档只提供一次性的手动配置映射说明。

## 实施计划

1. 在 workspace 中添加 `crates/agent-config`，依赖仅限于 TOML 反序列化、错误
   处理、URL 校验、用户主目录发现和必要的 tracing 过滤解析。
2. 实现原始 TOML schema、公开的类型化配置模型、路径解析、默认值处理和密钥
   脱敏的 `ConfigError` 变体。
3. 增加单元测试，覆盖文件缺失/不可读、TOML 语法失败、未知键、必填键缺失、
   当前默认值、自定义合法配置、空 reasoning effort、非法枚举、URL 校验和 API
   Key 脱敏。
4. 用 `agent-config` 加载替换 `agent-cli` 内部环境变量读取和 `Config`。在 CLI
   组合根部转换配置枚举，并通过类型化日志配置初始化 tracing。
5. 移除 `dotenvy`、`.env` 启动调用、环境变量读取辅助函数和环境变量专用测试。
   确认应用配置路径不再使用 `std::env`。
6. 更新 `README.md`、`docs/CONTRIB.md` 和 `docs/RUNBOOK.md`，包含 TOML schema、
   配置创建说明、准确路径、故障排查信息与新的发布二进制行为。不得提交含凭据的
   配置文件。
7. 执行验证、格式化代码，并使用临时主目录或显式测试路径 API 完成手动冒烟测试，
   避免自动化测试依赖真实用户配置。

## 验证清单

- `cargo fmt --check`
- `cargo test -p agent-config`
- `cargo test -p agent-cli`
- `cargo test`
- `cargo clippy --workspace --all-targets -- -D warnings`
- 使用有效的 `~/.mini-agent/config.toml` 执行手动启动测试。
- 在配置文件缺失、格式错误，以及 `api_type`、`reasoning_effort`、日志配置非法
  时执行手动启动测试。
- 确认仓库 `.env` 和导出的 `OPENAI_*` 变量不会改变实际生效配置。
- 确认非法配置错误和 trace 输出均不会泄露 API Key。

## 风险与回滚

- **内测配置未创建**：首次启动可能因缺少配置文件失败。应提供清晰的错误消息和
  最小配置模板，帮助内测用户完成初始化。
- **密钥泄露**：若用户将 TOML 放在仓库内，可能被意外提交。固定的用户主目录
  路径和文档可降低风险；错误信息和 Debug 格式化必须持续脱敏。
- **主目录边界情况**：容器或服务账户可能没有可解析的主目录。应返回明确的路径
  解析错误，不得静默回退到当前目录。
- **日志配置表达能力**：日志级别字段需要支持比单一级别更丰富的 directive。
  TOML 的 `logging.level` 字段继续接受 `EnvFilter` directive 语法。

回滚仅需回退应用代码和二进制版本。由于本次不提供旧配置兼容层，回滚设计不包含
新旧配置格式的双向转换；必要时由开发者手动维护对应版本的本地配置文件。
